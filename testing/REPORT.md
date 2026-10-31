# Отчёт: прохождение pjdfstest и xfstests для mirror-fs поверх nfs-mamont

**Дата:** 2026-10-31
**Объект:** `mirror-fs` (ветка `claude/bold-cerf-enua1c`, HEAD `606ef0b`) поверх
`nfs-mamont 0.2.0` (crates.io), а также проверка сборки против `nfs-mamont`
`master` (HEAD `5eae9a5`).
**Протокол:** NFSv3 over TCP + MOUNTv3 (NLM не тестировался — ещё в разработке).

---

## 0. TL;DR

- **Транспорт/протокол работают.** NFSv3-клиент ядра Linux монтирует экспорт
  mirror-fs, базовые операции (create/lookup/read/write/readdir/rename/symlink/
  hardlink/truncate/setattr-time), чтение/запись больших и разреженных файлов,
  целостность данных — всё проходит. Пропускная способность ~25–35 МБ/с (это
  потолок стенда на TCG-эмуляции, не mirror-fs).
- **Концептуальный разрыв №1 — идентичность вызывающего игнорируется.**
  Все операции ФС выполняются от имени процесса-сервера (root). UID/GID
  NFS-клиента нигде не применяется: нет `setfsuid`/`seteuid`, а `chown`
  запрещён полностью. Следствие: любой созданный файл принадлежит root,
  сменить владельца нельзя. Это корень большинства провалов pjdfstest.
- **Концептуальный разрыв №2 — root (`--no-root-squash`) не обходит права.**
  root не может прочитать файл `0000` или записать в `0444`, хотя на реальном
  NFS-сервере с `no_root_squash` это разрешено.
- **READDIR/READDIRPLUS не возвращают `.` и `..`** — нарушение POSIX и RFC 1813;
  `ls -a` в каталоге не показывает точечных записей.
- **Спецфайлы (MKNOD: fifo/block/char/socket) не поддерживаются** — это
  осознанный пропуск фичи (корректно отдаётся `NFS3ERR_NOTSUPP`), но pjdfstest
  массово падает на нём.
- **FSSTAT/FSINFO возвращают нули** — `df` показывает размер 0; как побочный
  эффект ломается автоопределение типа ФС в xfstests (нужен обход).
- **pjdfstest (частичный прогон, 5588+ подтестов):** ~29% pass. Разбивка и
  первопричины ниже — почти всё сводится к 4–5 дефектам в `mirror-fs`, не в
  `nfs-mamont`.
- **xfstests (FSTYP=nfs):** harness заводится; применимые к NFSv3 тесты
  проходят, большинство — `[not run]` из-за ограничений NFSv3/непокрытых фич
  (fallocate/punch, xattr/ACL, block-scratch, O_TMPFILE, renameat2 и т.п.).
- **Сборка:** релизная сборка mirror-fs против `nfs-mamont master` проходит;
  **debug-сборка ломается** — `nfs_mamont::init_tracing` удалён в nfs-mamont,
  а `mirror-fs/src/main.rs` его ещё импортирует под `#[cfg(debug_assertions)]`.

> Severity-пометки ниже: **[crit]** — грубое нарушение семантики/заметно для
> обычных приложений; **[major]** — заметное несоответствие; **[minor]** —
> краевой случай/ограничение протокола.

---

## 1. Стенд и методология

Это нетривиальная часть: **в контейнере сессии нет NFS-клиента в ядре**
(`/proc/filesystems` не содержит `nfs`), нет модулей и нет `/dev/kvm`. Поэтому
NFS-клиент был поднят в виртуальной машине.

- **Гость:** QEMU `q35`, ускоритель **TCG** (KVM недоступен), ядро Ubuntu HWE
  **7.0.0-42-generic** (из `.deb`, модули `nfs`/`nfsv3`/`lockd`/`sunrpc`/`9p`
  собраны как модули и подгружаются из initramfs).
- **Корень гостя:** хостовый `/` проброшен через **virtio-9p**
  (`cache=loose`), так что в VM доступны все инструменты, собранные на хосте
  (pjdfstest, xfstests, prove, python3 и т.д.). Отдельный 9p-шар
  (`cache=none`) — для подачи заданий и сбора логов.
- **Сеть:** host↔guest через `tap` (хост `10.10.1x.1`, гость `10.10.1x.2`),
  mirror-fs слушает на хосте, гость монтирует
  `mount -t nfs -o vers=3,proto=tcp,port=<p>,mountport=<p>,mountproto=tcp,nolock`.
- **Запуск mirror-fs:** `--no-root-squash` (чтобы UID 0 клиента был
  привилегированным — это *выгодный для mirror-fs* режим; при root-squash
  провалов было бы больше).
- **Базовая линия:** в отдельной VM поднят **настоящий Linux knfsd**
  (`rpc.nfsd`/`rpc.mountd`, экспорт ext4 через loopback, тот же NFSv3), чтобы
  отделить дефекты mirror-fs от «неизбежного на любом NFSv3».

Из-за TCG (без KVM) всё идёт в десятки раз медленнее реального железа; часть
прогонов на момент написания ещё не завершилась — такие места явно помечены
как «частичные». Скрипты стенда — в `testing/harness/`, сырые логи — в
`testing/results/`.

### Что заведомо работает (smoke + нагрузка)

- mount/umount, MOUNT-протокол, регистрация в rpcbind.
- create/lookup/read/write/rename/symlink/hardlink/mkdir/rmdir/readdir.
- Запись/чтение файла 5 ГБ, разреженные файлы (дыры читаются нулями, `st_blocks`
  корректны), целостность после `echo 3 > drop_caches` (md5 совпадает).
- Листинг каталога из 3000 и 500 файлов — все уникальны, пагинация по cookie
  корректна. `rm -rf` большого дерева.
- Длинные имена (255 байт — ок, 256 — `ENAMETOOLONG`), UTF-8 и имена со
  спецсимволами. O_EXCL (повторный — `EEXIST`). Переименование каталога на
  пустой — ок, на непустой — `EEXIST`. Hardlink переживает unlink оригинала.
- suid/sgid/sticky-биты (`chmod 7755`) сохраняются; `nlink` корректен.

---

## 2. pjdfstest — результаты

Частичный прогон (идёт медленно из-за TCG). На момент снятия среза — **5588
подтестов**, из них пройдено ~1630 (**~29%**). Разбивка по категориям
(подтесты):

| категория        | pass | fail | %pass | основная причина падений |
|------------------|-----:|-----:|------:|--------------------------|
| chflags          |   14 |    0 | 100%  | (нет chflags в Linux → skip) |
| chmod            |  140 |  187 |  42%  | F2 (root не обходит права), F5 (chmod требует W) |
| chown            |  338 | 1159 |  22%  | **F1 (chown запрещён) + F-OWN (владелец всегда root)** |
| ftruncate        |   73 |   16 |  82%  | F2 (truncate 0444 владельцем-root) |
| granular         |    7 |    0 | 100%  | — |
| link             |  158 |  201 |  44%  | спецфайлы (F3), права (F2/F5) |
| mkdir            |   73 |   45 |  61%  | владелец/права |
| mkfifo           |   45 |   75 |  37%  | **F3 (спецфайлы не поддержаны)** |
| mknod            |   57 |  129 |  30%  | **F3** |
| open             |  123 |  214 |  36%  | права (F2/F5), владелец |
| posix_fallocate  |    1 |    0 | 100%  | — |
| rename           |  601 | 1932 |  23%  | каскад F1/F3 в setup тестов |
| **ИТОГО**        | 1630 | 3958 |  29%  | |

**Базовая линия knfsd (та же версия pjdfstest, тот же клиент):** на уровне
файлов тестов `chmod/00..12` — все `ok` (там, где mirror-fs валит десятки
подтестов). Это подтверждает: перечисленные ниже падения — **дефекты
mirror-fs, а не особенности NFSv3**. (Полный прогон базовой линии на TCG не
завершился к моменту отчёта; файл — `testing/results/pjd-knfsd.tap`.)

---

## 3. Первопричины (с привязкой к коду и минимальными репро)

Все репро выполнены от **root** клиента при `--no-root-squash` (т.е. в самом
благоприятном для mirror-fs режиме). Полные логи:
`testing/results/probe-conformance.log`, `probe-permissions.log`,
`probe-semantics.log`.

### F1 [crit] — `chown` запрещён полностью (включая no-op)

`SETATTR` с установленными `uid`/`gid` всегда отвергается, даже если владелец
не меняется:

```
# chown 0:0 f        -> Invalid argument
# chown 65534 f      -> Invalid argument
```

Код: `src/fs/mod.rs:345` (`fn apply_set_attr`):

```rust
if new_attr.uid.is_some() || new_attr.gid.is_some() {
    return Err(vfs::Error::InvalidArgument);
}
```

NFS-клиент Linux отправляет `uid`/`gid` в `SETATTR` не только на `chown`, но и
в ряде других сценариев, поэтому эффект шире, чем просто неработающий `chown`.
Ожидаемо: no-op chown (значение = текущему) должен проходить; смена владельца
— выполняться (или отдавать `EPERM`, но не `EINVAL`).

### F-OWN [crit] — владелец создаваемых объектов всегда = процесс-сервер (root)

Файл, созданный клиентом с `uid=65534`, на бэкенде принадлежит `0:0`:

```
$ setpriv --reuid=65534 --regid=65534 sh -c 'echo hi > byNobody'
$ ls -ln byNobody   ->   -rw-r--r-- 1 0 0 ...   # ожидалось 65534 65534
```

Причина: mirror-fs выполняет все операции от идентичности своего процесса и
нигде не применяет учётные данные вызывающего (во всём дереве `src/` нет
`setfsuid`/`seteuid`/`chown`-к-владельцу). Вместе с F1 это значит, что
отразить владельца NFS-клиента в бэкенд-ФС **невозможно в принципе**. На
реальном knfsd с `no_root_squash` сервер делает `setfsuid/setfsgid` в учётку
вызывающего, и владелец получается правильным.

### F2 [crit] — root (`no_root_squash`) не обходит биты прав

```
# echo a > ro; chmod 0444 ro; dd if=/dev/zero of=ro ... -> Permission denied
# echo s > z;  chmod 0000 z;  cat z                      -> Permission denied
```

Код: `src/auth/posix.rs:111` (`fn privileged_mode_allows`): привилегированному
root доступ даётся, только если нужный бит выставлен хотя бы в одном классе
(`union`), т.е. при `0000`/`0444` запись/чтение root'у запрещаются. На
реальном NFS с `no_root_squash` root обходит rwx (для чтения/записи и обхода
каталогов; на исполнение обычного файла всё же нужен хоть один бит `x`).

### F5 [major] — `chmod`/`utimes` требуют права W на объекте

`SETATTR` защищён `require_write` (`src/fs/set_attr_impl.rs:12`). Но POSIX
`chmod`/`utimes(_, NULL)` требуют **владения** объектом, а не права записи:

```
# touch m; chmod 0000 m; chmod 0644 m   -> Permission denied (на втором chmod)
```

Владелец должен иметь право снять/поставить биты на своём же `0000`-файле.
Сейчас семантика «изменить данные» (W) смешана с «изменить метаданные»
(владение). В связке с F2 это бьёт и по root.

### F3 [major, feature-gap] — спецфайлы не создаются (MKNOD)

`mkfifo`, `mknod b/c`, `bind()` unix-сокета → `NFS3ERR_NOTSUPP` (у клиента это
`errno 524` / «Unknown error 524»). Код: `src/fs/mk_node_impl.rs:13`
(`vfs::Error::NotSupported`). С точки зрения протокола это **корректно**
(сервер честно отдаёт NOTSUPP), но pjdfstest трактует это как провал и, что
важнее, каскадно валит `rename`/`chmod`/`chown`/`unlink`-тесты, которые в
setup создают спецфайлы. Поддержка `mknod(2)`/`mkfifo(2)` на бэкенде
несложна и закрыла бы большой пласт тестов.

### F4 [crit] — READDIR/READDIRPLUS не отдают `.` и `..`

```
$ mkdir d; ls -a1 d        # пусто: нет ни '.', ни '..'
```

Код: `src/fs/read_dir_impl.rs:33` и `read_dir_plus_impl.rs` строят список
только из `list_directory_entries` (`src/fs/mod.rs:392`, это
`std::fs::read_dir`, который `.`/`..` не включает), и точечные записи нигде не
добавляются. RFC 1813 и POSIX требуют их в листинге каталога. `LOOKUP` для
`.`/`..` при этом работает (`src/fs/lookup_impl.rs`), т.е. дефект именно в
перечислении. Ломает приложения, полагающиеся на `.`/`..` в выдаче
(обход деревьев, подсчёт `nlink` каталога и т.п.).

### F6 [minor] — FSSTAT/FSINFO возвращают нули

`src/fs/fs_stat_impl.rs:18` — `total_bytes/free_bytes/...= 0`. `df` показывает
нулевой размер. Отдельно это сломало автоопределение ФС в xfstests
(`df` без `-a` скрывает mount с нулевым размером → `FSTYP` не детектится;
обход — `DF_PROG="df -T -P -a"`). Стоит отдавать реальные значения
`statvfs(2)` с бэкенда.

### F7 [minor] — усечение времён до диапазона `u32`

`src/fs/mod.rs:232` (`time_from_unix`): время до 1970 → `0`, после 2106 →
`u32::MAX`. Частично это ограничение NFSv3 (`nfstime3.seconds` — `uint32`), но
выбор «насыщать» (а не отражать ошибку/обрезать иначе) влияет на тесты времён.

### Прочие наблюдения (не подтверждены полным репро)

- Verifier EXCLUSIVE-create пишется в `mtime` файла (`store_exclusive_verifier`)
  и не очищается — до первого `SETATTR` `GETATTR` вернёт «мусорный» mtime.
- Cookie-verifier в READDIR основан на `ctime` каталога: при изменении каталога
  между запросами страниц возможен `NFS3ERR_BAD_COOKIE` на валидной итерации.

---

## 4. xfstests (FSTYP=nfs)

Harness заводится против mirror-fs (после обхода F6). Прогон идёт на TCG
медленно; на момент среза: из применимых тестов группы `quick`, которые
реально исполнились, **падений не зафиксировано**, значительная часть —
`[not run]`. Типичные причины `[not run]` (ожидаемы для базового NFSv3-сервера):

- `require .../scratch to be valid block disk` — многим тестам нужен
  блочный scratch-девайс (у NFS его нет);
- `xfs_io fpunch/fzero/fcollapse/falloc/fiemap failed` — нет
  FALLOCATE/hole-punch (в NFSv3 отсутствует как класс);
- `nfs does not support shutdown`, `defragmentation not supported`,
  `O_TMPFILE is not supported`, `renameat2 syscall`, `atime mount options` —
  неприменимо к NFSv3;
- `attr namespace user not supported`, `ACLs not supported` — xattr/ACL не
  экспонируются NFSv3-клиентом.

Вывод: для NFSv3-сервера такой профиль `[not run]` нормален; важны
data-integrity тесты (fsx/fsstress) — для них запущен отдельный
курированный список (`testing/harness`, порт 2051). Частичные выводы check —
`testing/results/xfs-quick-partial-shard*.out`.

> Примечание: xfstests гоняет сам протокол+клиент ядра поверх mirror-fs; он
> слабо покрывает POSIX-семантику прав/владельца — именно поэтому pjdfstest
> оказался куда более показательным для дефектов mirror-fs.

---

## 5. Атрибуция: mirror-fs vs nfs-mamont

Практически все найденные дефекты — в **mirror-fs** (слой VFS-бэкенда):
F1, F-OWN, F2, F5, F6, F7 — это его `src/fs/*` и `src/auth/*`. F3 — осознанный
feature-gap бэкенда (библиотека корректно несёт `NFS3ERR_NOTSUPP`). F4 —
тоже mirror-fs (не синтезирует `.`/`..`), хотя как вариант их можно было бы
добавлять и на уровне сериализатора READDIR в nfs-mamont.

Со стороны **nfs-mamont** в прогонах протокольных дефектов не проявилось:
парсинг/сериализация XDR, RPC, MOUNT и диспетчеризация NFSv3 отработали на
тысячах операций без сбоев. (Мелочь-наблюдение: оценка размера ответа
READDIRPLUS в сериализаторе приблизительная — для очень больших каталогов
стоит убедиться, что ответ не превышает `maxcount` клиента; на 3000 файлов
проблем не наблюдалось.)

---

## 6. Сборка mirror-fs против nfs-mamont `master`

- **Release:** `cargo build --release` с
  `--config 'patch.crates-io.nfs-mamont.path="…/nfs-mamont"'` — **ок**.
- **Debug:** **ломается** — `src/main.rs:13` импортирует
  `use nfs_mamont::init_tracing;` под `#[cfg(debug_assertions)]`, а в
  `master` этот публичный хелпер удалён (вынос `tracing-subscriber` из
  библиотеки, nfs-mamont #434). Нужно либо вернуть инициализацию трейсинга в
  само приложение, либо убрать этот вызов. На релизной сборке (которой
  тестировали) не проявляется, т.к. код под `debug_assertions`.

---

## 7. Приоритеты исправлений (предложение)

1. **F-OWN + F1** — применять учётные данные вызывающего к операциям бэкенда
   (`setfsuid/setfsgid` в рамках VFS-пула) и/или разрешить `chown`; без этого
   модель владельца не соответствует NFS в принципе. *Снимет бóльшую часть
   провалов chown/open/rename/link.*
2. **F2 + F5** — корректная модель прав: привилегированный root обходит rwx;
   `chmod`/`utimes` проверять владение, а не право W.
3. **F4** — добавлять `.` и `..` в READDIR/READDIRPLUS.
4. **F3** — реализовать MKNOD (fifo/char/block/socket) на бэкенде.
5. **F6** — отдавать реальные значения `statvfs` в FSSTAT/FSINFO.
6. Починить debug-сборку против nfs-mamont master (F-build).
7. **F7** и краевые (EXCLUSIVE verifier, cookie-verifier) — по остаточному
   принципу.

---

## 8. Как воспроизвести

Скрипты и конфиги — в `testing/harness/` (сборка UML/QEMU-гостя, запуск
mirror-fs, подача заданий в VM, раннеры pjdfstest и xfstests, базовая линия
knfsd). Кратко:

1. Собрать гостевое ядро/initramfs (`kernel .deb` + busybox + 9p-модули).
2. Поднять `tap`, запустить `mirror-fs -c <config> -a 0.0.0.0:<port> --no-root-squash`.
3. Загрузить VM (`start-vm2.sh`), в ней смонтировать NFSv3 и запустить
   `prove -rv /opt/src/pjdfstest/tests` и/или `./check -g quick` (xfstests,
   `local.config` с `DF_PROG="df -T -P -a"`).

Сырые результаты — в `testing/results/`.
