# RAG и веб‑UI: архитектура и фазы

Цель: **не** раздувать `claw-analog` и основной `claw` — вынести индексацию и (позже) UI в отдельные процессы с явными HTTP/MCP контрактами.

## Принципы

1. **RAG как сервис** — отдельный бинарь (сейчас `claw-rag-service`), свой жизненный цикл, свои секреты (embedding API), своё хранилище.
2. **Агент только вызывает retrieval** — в **`claw-analog`** инструмент **`retrieve_context`** → HTTP `POST {RAG_BASE_URL}/v1/query` (база без суффикса `/v1`); лимиты **`rag_timeout_secs`**, **`rag_top_k_max`** в `.claw-analog.toml`; ответ для модели — фрагменты с `path` + `snippet` + `score`.
3. **Веб‑UI** — минимальная страница **`GET /`** в `claw-rag-service` (stats + форма `POST /v1/query`); чат с моделью и «переиндексировать» из браузера — при необходимости позже.

## Компоненты (целевая картина)

```text
┌─────────────────┐     POST /v1/query      ┌──────────────────────┐
│  claw-analog    │ ──────────────────────►│  claw-rag-service    │
│  (+ tool)       │◄──────────────────────│  (embed + vector DB) │
└─────────────────┘     JSON hits          └──────────┬───────────┘
                                                      │
                                             ingest (watch / CLI)
                                                      ▼
                                             workspace files / git tree
```

- **Индексация**: отдельная команда или воркер (chunking, хеш файла, инкремент). Хранилище: на старте SQLite + `sqlite-vec` / файловый эмбеддинг-кэш; при росте — Qdrant/Chroma в Docker.
- **Эмбеддинги**: HTTP к OpenAI/Anthropic-совместимому embedding endpoint или локальная модель (отдельное решение по лицензии и размеру).
- **Веб‑UI**: авторизация (минимум: токен + reverse proxy), SSE или WebSocket для стрима ответа модели; UI **не** владеет секретами провайдера, если продукт так решит — прокси через бэкенд.

## Текущая реализация

Крейт **`rust/crates/claw-rag-service`** (из каталога `rust/`):

### HTTP

- `GET /` — одностраничный UI (встроенный `static/index.html`): счётчики из `/v1/stats`, поиск через `/v1/query`.
- `GET /health` — `ok`.
- `GET /v1/stats` — `{ "chunks": N, "phase": "1-sqlite" }` (если БД ещё нет: `chunks: 0`, `phase`: `1-sqlite-no-db`).
- `POST /v1/query` — тело `{"query":"...", "top_k":8}`; ответ `{"hits":[{"path","snippet","score"}], "phase":"1-sqlite"|"1-sqlite-empty"|"1-sqlite-no-db"}`.

Поиск: **линейный обход** всех векторов в SQLite (MVP; для больших репозиториев планировать Qdrant/sqlite-vec или батчевый ANN).

### Индексация (фаза 1)

```powershell
cd D:\path\to\claw-code-main\rust
$env:OPENAI_API_KEY = "sk-..."
cargo run -p claw-rag-service -- ingest -w D:\path\to\repo --db D:\path\to\index.sqlite
cargo run -p claw-analog -- ...   # при RAG_BASE_URL или rag_base_url в TOML — инструмент retrieve_context
```

Переменные окружения:

- **`OPENAI_API_KEY`** или **`CLAW_RAG_OPENAI_API_KEY`** — для вызова `POST …/embeddings`.
- **`CLAW_RAG_EMBEDDING_BASE_URL`** — по умолчанию `https://api.openai.com/v1`.
- **`CLAW_RAG_EMBEDDING_MODEL`** — по умолчанию `text-embedding-3-small`.
- **`CLAW_RAG_DB`** — путь к SQLite (у ingest/`serve`; у `serve` есть default `.claw-rag/index.sqlite`).
- **`CLAW_RAG_PORT`** — порт HTTP (по умолчанию `8787`).
- **`CLAW_RAG_MOCK_PROVIDERS=1`** — детерминированные вектора без сети (для тестов CI).

Запуск сервера: `cargo run -p claw-rag-service` или `cargo run -p claw-rag-service -- serve --db path\to\index.sqlite`.

### Дальше по фазам

| Фаза | Содержание |
|------|------------|
| 1 | ~~Ingest + SQLite + embeddings~~ (базово сделано; улучшения: инкремент, ANN, Docker-векторка). |
| 2 | ~~Инструмент `retrieve_context`~~: `RAG_BASE_URL` / `rag_base_url`, `rag_timeout_secs`, `rag_top_k_max` в `.claw-analog.toml`. |
| 3 | ~~Минимальный UI~~: `GET /` + те же `/v1/*` (дальше: чат, кнопка re-index из UI). |

## Риски и ограничения

- Секреты и PII в индексе; размер индекса и стоимость эмбеддингов.
- Согласованность с symlink/jail как в `claw-analog` — retrieval не должен «утекать» за пределы workspace.
- Локаль на UI: i18n отдельно от `AnalogLanguage` в CLI.

## Связанные документы

- Локальный запуск контейнеров (если поднимете векторку): [`container.md`](container.md).
- Обзор `claw-analog`: [`how_to_run.md`](../how_to_run.md).
