# claw-analog — как запускать и как это устроено

Минимальный агент поверх того же стека API, что и основной CLI [`claw`](rust/README.md): провайдеры Anthropic / OpenAI‑совместимые / xAI выбираются по модели и переменным окружения (см. [USAGE.md](USAGE.md)).

Дальше в примерах **рабочий каталог** — папка **`claw-code-main\rust`** (внутри клона репозитория). Если приглашение PowerShell уже `…\claw-code-main\rust>`, **не** выполняйте второй раз `cd rust` (иначе будет `rust\rust` и ошибка пути).

## Требования

- Установленный **Rust** и **cargo** (в PATH: обычно `%USERPROFILE%\.cargo\bin` на Windows).
- Ключ API для выбранного провайдера (например `ANTHROPIC_API_KEY`).

## Сборка и справка

```powershell
cd D:\path\to\claw-code-main\rust
cargo build -p claw-analog
cargo run -p claw-analog -- --help
```

### Диагностика (`doctor`)

Подкоманда **`claw-analog doctor`** (у неё свой `--help`, отдельно от основного режима):

- **превью конфигурации** — итог после слияния **`.claw-analog.toml`** (путь `<workspace>/.claw-analog.toml` или **`--config`**) и **тех же флагов**, что у основного run: **`--model`**, **`--permission`**, **`--preset`**, **`--output-format`**, **`--stream`**, **`--no-stream`**, **`--no-runtime-enforcer`**, **`--accept-danger-non-interactive`**, плюс **`--profile`** для отображения пути к профилю. Печатаются контракт NDJSON (`schema`, `format_version`), эффективные поля и строки **provenance** (что победило: CLI, TOML или default);
- статус типовых переменных (**без** значений: только `set` / `unset` и длина строки);
- поиск workspace вверх от cwd (или **`--manifest-dir`**) и по умолчанию **`cargo check -p claw-analog`** (только компиляция, **не** перезаписывает `target\debug\claw-analog.exe` — иначе на Windows при `cargo run … doctor` часто «Отказано в доступе» при вложенном `cargo build`);
- **`--release-build`** — **`cargo build --release -p claw-analog`** (бинарь в `target\release\`, не конфликтует с запущенным debug‑exe);
- **`--no-build`** — пропустить cargo;
- **`--tcp-ping`** (алиас **`--mock`**) — TCP **`connect`** к хосту:порту из **`ANTHROPIC_BASE_URL`** (или к дефолтному `https://api.anthropic.com`); не проверяет HTTP/TLS и тело ответа.

Примеры (из каталога `…\claw-code-main\rust`):

```powershell
cargo run -p claw-analog -- doctor
cargo run -p claw-analog -- doctor --no-build
cargo run -p claw-analog -- doctor --tcp-ping
cargo run -p claw-analog -- doctor -w D:\path\to\repo --preset implement
cargo run -p claw-analog -- doctor --release-build
```

### Проверка конфигурации без API (`config validate`)

Подкоманда **`claw-analog config validate`**:

- парсит **`.claw-analog.toml`** (по умолчанию `<workspace>/.claw-analog.toml`, переопределение **`--config`**) и выводит краткий **merge preview** (как у `doctor`, но **только TOML + defaults**, без флагов основного run);
- проверяет **`profile.toml`**: тот же порядок, что у run (`--profile`, поле `profile` в TOML, иначе дефолтный `~/.claw-analog/profile.toml` при наличии файла);
- **никаких** запросов к LLM и сети API.

**`--strict`** — ошибка (код выхода 1), если файла конфигурации нет или профиль не читается.

```powershell
cargo run -p claw-analog -- config validate -w D:\path\to\repo
cargo run -p claw-analog -- config validate --strict -w .
```

### Дополнение оболочки (`complete`)

Скрипт автодополнения в **stdout** (перенаправьте в файл из документации вашей оболочки):

```powershell
cargo run -p claw-analog -- complete powershell >> $PROFILE
# bash:zsh:fish — см. вывод `complete --help`
```

Доступные значения: **`bash`**, **`zsh`**, **`fish`**, **`powershell`** (алиас **`pwsh`**).

## Основные команды

Одна задача в аргументе (или текст с **stdin**):

```powershell
# из ...\claw-code-main\rust
cargo run -p claw-analog -- -w D:\path\to\repo "Кратко опиши структуру rust/crates"
```

С **живым выводом** (SSE через `stream_message`):

```powershell
cargo run -p claw-analog -- --stream -w . "Объясни claw-analog в двух предложениях"
```

Разрешить **запись файлов** в workspace:

```powershell
cargo run -p claw-analog -- --permission workspace-write -w . "Добавь комментарий в начало crates/claw-analog/Cargo.toml"
```

Отключить проверку через **`runtime::PermissionEnforcer`** (только своя тюрьма путей; не рекомендуется):

```powershell
cargo run -p claw-analog -- --no-runtime-enforcer -w . "…"
```

Полезные лимиты (CLI **перекрывает** значения из `.claw-analog.toml`, см. ниже):

| Флаг | Значение по умолчанию | Назначение |
|------|------------------------|------------|
| `--max-read-bytes` | 262144 | Максимум байт для `read_file` / `grep_workspace` / `git_diff` / `git_log` |
| `--max-turns` | 24 | Максимум раундов «модель → инструменты → модель» |
| `--max-list-entries` | 500 | Лимит строк `list_dir` |
| `--grep-max-lines` | 200 | Верхняя граница **суммарных** строк совпадений в `grep_workspace` (в т.ч. по нескольким файлам; в одном файле можно задать меньше через `max_lines`) |
| `--glob-max-paths` | 2000 | Максимум путей, возвращаемых `glob_workspace` и при расширении `glob` внутри `grep_workspace` |
| `--glob-max-depth` | 32 | Глубина обхода каталогов для glob (через `walkdir`), без бесконечной рекурсии |
| `--output-format` | `rich` | `json` — NDJSON на stdout для скриптов и агентов |
| `--print-tools` | — | Список эффективных инструментов для итоговых `permission` / enforcer, затем выход (**без** промпта и API) |
| `--lang` | `en` | Подсказка в system: `en` или `ru` (язык ответов; **не** меняет id модели в API) |
| `--preset` | — | `none` \| `audit` \| `explain` \| `implement` — см. раздел ниже |
| `--session` | — | Путь к JSON-сессии (относительно `-w`, если не абсолютный): сохранение истории и resume |
| `--save-session` | — | Дополнительный путь: тот же снимок сессии пишется сюда при каждом сохранении (можно **без** `--session`, чтобы только экспортировать JSON после прогона) |
| `--profile` | — | TOML с полем `line` (подмешивается в system). Без флага: пробуется `%USERPROFILE%\.claw-analog\profile.toml` (Windows) / `~/.claw-analog/profile.toml` |
| `--permission` | `read-only` | см. ниже: `read-only`, `workspace-write`, `prompt`, `danger-full-access`, `allow` |
| `--accept-danger-non-interactive` | — | Разрешить `danger-full-access` / `allow`, когда stdin **не** TTY (CI; осознанный риск). В TOML: `accept_danger_non_interactive = true` |

Конфиг по умолчанию читается из **`<workspace>/.claw-analog.toml`**, если файл существует. Другой путь: **`--config PATH`**. Неизвестные ключи в TOML — ошибка парсинга (строгая схема).

Пример `.claw-analog.toml`:

```toml
model = "sonnet"
stream = true
output_format = "rich"
permission = "read-only"
language = "en"
preset = "audit"
session = ".claw-analog.session.json"
profile = "~/.claw-analog/profile.toml"
no_runtime_enforcer = false
accept_danger_non_interactive = false
max_read_bytes = 262144
max_turns = 24
max_list_entries = 500
grep_max_lines = 200
glob_max_paths = 2000
glob_max_depth = 32
# Опционально: RAG (`claw-rag-service`) — см. раздел про RAG ниже
# rag_base_url = "http://127.0.0.1:8787"
# rag_timeout_secs = 30
# rag_top_k_max = 32
```

**RAG (`retrieve_context`):** если заданы **`RAG_BASE_URL`** (per-env) или непустой **`rag_base_url`** в `.claw-analog.toml`, в набор инструментов добавляется **`retrieve_context`** (семантический поиск по уже проиндексированному воркспейсу). Значение — корень HTTP сервиса, без суффикса `/v1` (запрос идёт на `{base}/v1/query`). Таймаут и верхняя граница **`top_k`** задаются **`rag_timeout_secs`** и **`rag_top_k_max`** (по умолчанию 30 с и 32; «жёсткий» потолок 256). Индексация по-прежнему отдельной командой **`claw-rag-service`**, см. [`docs/rag-web-ui.md`](docs/rag-web-ui.md).

**`permission`** (как у полного `claw`, те же строки в TOML):

| Значение | Инструмент `write_file` | Неинтерактив (stdin не TTY) |
|----------|-------------------------|------------------------------|
| `read-only` | нет | OK |
| `workspace-write` | да (в пределах `-w`) | OK |
| `prompt` | нет (в этом harness Enforcer не даёт писать без подтверждений) | предупреждение в stderr; для автозаписи используйте `workspace-write` |
| `danger-full-access`, `allow` | да | **запрещено**, пока не задан `--accept-danger-non-interactive` или `accept_danger_non_interactive = true` в TOML |

**`--stream`** в командной строке включает стриминг; **`--no-stream`** явно выключает (полезно поверх `stream = true` в файле).

**`language`** в TOML: `en` или `ru` (те же значения, что у **`--lang`**); CLI имеет приоритет.

### Сессия (`--session`)

Файл JSON (версия `1`): метаданные `workspace`, `model`, опционально `preset`, массив `messages` в формате API (`role` + `content`). При запуске с существующим файлом история **догружается**, текущий текст запроса (аргумент или stdin) добавляется как **новое** пользовательское сообщение. Состояние сохраняется после каждого полного раунда с инструментами и при завершении без `tool_use`.

**`--save-session`** — тот же формат файла, что и у `--session`: при каждом шаге, где обновлялся бы файл сессии, запись дублируется (если путь совпадает с `--session`, вторая запись не выполняется). Без **`--session`** можно собрать историю одного прогона в JSON для скриптов или последующего **`--session`** без ручной сборки `messages`.

**Риски:** в файле могут оказаться **секреты** (вывод `read_file`, ключи из логов), файл не шифруется; длинная история **дороже** по токенам API. В stderr печатается напоминание при **`--session`** или **`--save-session`**. Несовпадение `workspace` / `model` / `preset` с текущим запуском даёт **предупреждение**, но прогон продолжается.

### Пресеты (`--preset`)

Добавляют краткий абзац к system prompt (аудит / обучение / правки). Набор инструментов по-прежнему задаётся **permission**: для **`implement`**, если ни CLI, ни файл не задали `permission`, по умолчанию подставляется **workspace-write** (чтобы был `write_file`). Явный `permission = "read-only"` в файле или `--permission read-only` в CLI имеет приоритет.

### Профиль (`profile.toml`)

Мини-файл:

```toml
line = "Короткая подсказка стиля (одна строка в system)."
```

Ограничения: размер файла не больше **2048** байт; длина строки после trim — не больше **512** символов Unicode (иначе усечение с предупреждением). Содержимое добавляется в system одной строкой: `Learner hint: …`.

## Инструменты (без произвольного shell)

| Имя | Режим | Описание |
|-----|--------|----------|
| `read_file` | read-only+ | Чтение UTF‑8 файла под `-w` |
| `list_dir` | read-only+ | Список каталога (не рекурсивно) |
| `glob_workspace` | read-only+ | Список **путей файлов** под `-w`: аргументы `pattern` (glob относительно `root`, слэши `/`), опционально `root` (по умолчанию `.`), `max_paths` (урезается лимитом CLI). В шаблоне нельзя `..`. |
| `grep_workspace` | read-only+ | Та же **литеральная** подстрока по строкам, что и раньше; ровно один из селекторов: `path`, массив `paths` или `glob` (+ опционально `glob_root`). Общий бюджет строк — `max_lines` и `--grep-max-lines`. В нескольких файлах формат строк: `относительный/путь:номер_строки:содержимое`. |
| `grep_search` | read-only+ | Тот же обработчик, что у `grep_workspace` (совместимость промптов с полным `claw`). |
| `git_diff` | read-only+ | `git diff` (без цвета) внутри репозитория в `-w`. Опционально `cached` (staged), `rev_range`, `context_lines`, `paths`. Вывод ограничен `--max-read-bytes`. |
| `git_log` | read-only+ | `git log` (без цвета) внутри репозитория в `-w`. Опционально `max_count` (по умолчанию 20), `rev_range`, `paths`. Вывод ограничен `--max-read-bytes`. |
| `retrieve_context` | read-only+ | Только если задан **`RAG_BASE_URL`** или **`rag_base_url`** в TOML: HTTP **`POST {base}/v1/query`** к `claw-rag-service`, ответ — пути и сниппеты чанков (лимиты см. выше). |
| `write_file` | `workspace-write`, `danger-full-access` или `allow` | Запись файла; родительские каталоги создаются при необходимости (`prompt` не даёт записать через Enforcer) |

## Принципы работы

1. **Корень workspace** (`-w`) приводится к каноническому пути; все пути в инструментах **относительные**, без `..` и без абсолютных сегментов.
2. Перед доступом к файлу проверяется, что реальный путь остаётся **внутри** корня (symlink/`canonicalize`).
3. **Политика прав** (если не отключена `--no-runtime-enforcer`): те же сущности, что у основного CLI — `PermissionPolicy` + `PermissionEnforcer::check` для инструмента и `check_file_write` для записи.
4. **Цикл агента**: запрос к провайдеру → если `stop_reason == tool_use`, выполняются вызовы, результаты уходят в историю как `tool_result` → следующий раунд.
5. **Стриминг**: при `--stream` текст ассистента печатается по мере прихода дельт; история для следующего раунда собирается из SSE так же, как в полном пайплайне (индексы блоков + JSON tool input). Отключить стриминг при настройке из файла можно флагом **`--no-stream`**.

Логи вида `[claw-analog] ...` пишутся в **stderr**. В режиме **rich** ответ модели — обычный текст в **stdout**; в режиме **json** в **stdout** идёт только **NDJSON** (см. ниже).

## Вывод JSON (CI и внешние агенты)

Флаг **`--output-format json`** переключает stdout на **поток строк JSON** (один объект = одна строка). Поля стабильны по смыслу, но набор может расширяться.

Основные `type`:

| `type` | Когда |
|--------|--------|
| `run_start` | Старт прогона: **`schema`** (`claw-analog-ndjson`), **`format_version`**, далее `workspace`, `model`, `stream`, `permission`, опционально `preset`, `session`, опционально `session_save`, булево **`rag_enabled`** (есть ли база для `retrieve_context`) |
| `turn_start` | Начало раунда с моделью (`turn`) |
| `assistant_text_delta` | Только при `--stream`: фрагмент текста ассистента |
| `assistant_turn` | Итог раунда: `stop_reason`, `usage`, полный `text`, массив `tool_calls` |
| `tool_result` | После выполнения инструмента: `name`, `tool_use_id`, `is_error`, `output` (может быть усечён), `truncated`, `output_len_chars` |
| `run_end` | Успешное завершение (`ok: true`) |
| `error` | Ошибка (печатается отдельной строкой при падении или пустом промпте) |

Пример (PowerShell): разбор потока построчно удобен **`jq`** или любом JSON‑парсере.

```powershell
# из ...\claw-code-main\rust
$env:ANTHROPIC_API_KEY = "sk-ant-..."
cargo run -p claw-analog -- --output-format json -w . "Summarize rust/README.md" 2>$null | ForEach-Object { $_ | ConvertFrom-Json | Select-Object -ExpandProperty type }
```

С **`--stream`** в stdout сначала идут события `assistant_text_delta`, затем для того же раунда — одна строка `assistant_turn` с полным собранным `text` (удобно для воспроизводимых логов).

### Ограничения и риски для агентов

- В **`tool_result.output`** большие файлы обрезаются (~32 KiB UTF‑8), поле **`truncated`: true**.
- **Секреты**: не перенаправляйте stderr сырьём в публичные логи без фильтра; в `output` теоретически может попасть содержимое прочитанных файлов.
- Контракт для оркестраторов: NDJSON из stdout, диагностика из stderr; код возврата ≠ 0 при ошибке. На первой строке **`run_start`** имеет смысл сверять **`schema`** и **`format_version`**; **`run_start`** также раскрывает путь workspace и модель — учитывайте при шаринге логов.

## Автотесты без реальной сети

Юнит‑тесты и интеграция с локальным **mock-anthropic-service**:

```powershell
# из ...\claw-code-main\rust
cargo test -p claw-analog
```

В **GitHub Actions** отдельный job **`claw-analog (test + clippy -p)`** гоняет `cargo test -p claw-analog` и `cargo clippy -p claw-analog --no-deps` (в дополнение к полному `cargo test` / `clippy` по workspace).

При параллельном запуске тестов переменные окружения Anthropic изолированы **mutex**‑ом только для mock‑сценария; при сбоях можно запустить `cargo test -p claw-analog -- --test-threads=1`.

## Отдельно: `claw-rag-service` (RAG)

Индексация воркспейса и HTTP API живут в **`cargo run -p claw-rag-service`** (`ingest` + `serve`). После `serve` откройте **`http://127.0.0.1:8787/`** — лёгкий UI (stats + поиск). К `claw-analog` подключается через **`RAG_BASE_URL`** / `retrieve_context`. Подробности и env: [`docs/rag-web-ui.md`](docs/rag-web-ui.md).

### Ingest (один или несколько репозиториев)

`ingest` принимает **повторяемый** `--workspace` — это позволяет сделать **cross-repo RAG** (несколько реп в одну БД/коллекцию).

```powershell
# из ...\claw-code-main\rust

# один workspace
cargo run -p claw-rag-service -- ingest --workspace "D:\v\kria\s6"

# несколько workspace (cross-repo)
cargo run -p claw-rag-service -- ingest --workspace "D:\repo1" --workspace "D:\repo2"
```

В ответах `path` будет вида `repoId:relative/path` (чтобы не было коллизий одинаковых путей между репозиториями).

### Mock embeddings (без ключей / без сети)

Для локальных прогонов/тестов можно включить mock-эмбеддинги:

```powershell
$env:CLAW_RAG_MOCK_PROVIDERS = "1"
cargo run -p claw-rag-service -- ingest --workspace "D:\v\kria\s6"
```

### Qdrant (рекомендуемый локальный вариант) через Docker

Для больших репозиториев лучше поднять локальный Qdrant: это снимает нагрузку с линейного сканирования `SQLite` и ускоряет запросы.

Запуск Qdrant (gRPC на 6334):

```powershell
docker run --rm -p 6333:6333 -p 6334:6334 -e QDRANT__SERVICE__GRPC_PORT=6334 qdrant/qdrant
```

#### Qdrant с persist volume (чтобы индекс сохранялся)

Вариант через именованный volume Docker:

```powershell
docker volume create claw-qdrant-data
docker run --rm -p 6333:6333 -p 6334:6334 `
  -e QDRANT__SERVICE__GRPC_PORT=6334 `
  -v claw-qdrant-data:/qdrant/storage `
  qdrant/qdrant
```

Вариант через bind-mount (путь на хосте):

```powershell
mkdir .claw-qdrant | Out-Null
docker run --rm -p 6333:6333 -p 6334:6334 `
  -e QDRANT__SERVICE__GRPC_PORT=6334 `
  -v "${PWD}/.claw-qdrant:/qdrant/storage" `
  qdrant/qdrant
```

Затем включите env и запускайте ingest с фичей `qdrant-index`:

```powershell
$env:CLAW_RAG_QDRANT_URL = "http://127.0.0.1:6334"
$env:CLAW_RAG_QDRANT_COLLECTION = "claw_rag_chunks"

# (опционально) без реального API для эмбеддингов
$env:CLAW_RAG_MOCK_PROVIDERS = "1"

cargo run -p claw-rag-service --features qdrant-index -- ingest --workspace "D:\v\kria\s6"
```

`ingest` сам создаст коллекцию, если её ещё нет (по размерности эмбеддингов).

### Запуск через Docker (Qdrant + claw-rag-service)

Если хочется поднимать всё одной командой, удобнее использовать `docker compose`.

1) Запуск сервисов:

```powershell
cd D:\path\to\claw-code-main
docker compose up --build
```

Примечание: образ `rag-serve`/`rag-ingest` собирается на достаточно свежем Rust (см. `rust/crates/claw-rag-service/Dockerfile`), потому что `qdrant-client` может требовать более новую версию Rust, чем старые pinned-теги.

Если сборка Docker падает и вы видите строки вроде `transferring context: 21.02GB`, проверьте что:

- вы запускаете compose из корня репозитория (где лежит `docker-compose.yml`)
- используется `.dockerignore` (уменьшает build-context, особенно если есть `target/` и локальные индексы)

Если сборка падает сразу с `EOF` на шаге `load local bake definitions`, попробуйте:

```powershell
$env:COMPOSE_BAKE = "0"
$env:DOCKER_BUILDKIT = "0"
docker compose up --build
```

2) Ingest (запускать отдельно, т.к. это batch job). Пример для одного workspace:

```powershell
docker compose run --rm rag-ingest ingest --workspace "/workspaces/main"
```

По умолчанию `rag-ingest` пишет индекс в общий volume, так что `rag-serve` сразу увидит чанки.

### Подключение к `claw-analog`

```powershell
$env:RAG_BASE_URL = "http://127.0.0.1:8787"
cargo run -p claw-analog -- -w "D:\v\kria\s6" "Найди где реализован ingest в RAG сервисе"
```

## Auto‑TDD (автопроверки после `write_file`/`edit_file`)

В полном `claw` (и в других потребителях `runtime`) можно включить автозапуск линтера/тестов после успешных write-инструментов через `.claw/settings.json`:

```json
{
  "autoTdd": {
    "enabled": true,
    "tools": ["write_file", "edit_file"],
    "commands": [
      "cd rust && cargo fmt",
      "cd rust && cargo clippy --workspace --all-targets -- -D warnings",
      "cd rust && cargo test --workspace"
    ]
  }
}
```

## Отличия от полного `claw`

- Узкий набор инструментов (нет bash/MCP/плагинов).
- Проще аудировать и ограничивать по `--permission` и лимитам.
- Основной продукт по-прежнему `cargo run -p rusty-claude-cli` → бинарь `claw`.

## Дальнейшая разработка

План и чеклист идей (в т.ч. заимствованные из продуктового слоя вроде DeepTutor): [`futute.md`](futute.md) в корне репозитория.
