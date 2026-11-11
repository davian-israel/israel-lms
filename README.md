# Margin (israel-lms)

An AI-assisted learning platform for UK pupils, Year 1 to Year 13. Pupils follow a daily plan built from spaced reviews (days 1, 3, 7, 14, 21), mistakes to fix and new lessons. Each lesson runs Read, Review, Reflect. Teachers see who knows what; parents see a weekly view with ways to help; safeguarding leads get alerts when a pupil writes something worrying.

Rust (Axum 0.8, Askama, SQLx) + PostgreSQL 16 + HTMX 4. One binary, `lms`, runs the web server, the background worker, or both.

## Run it locally

With Docker:

```sh
docker compose up --build
# open http://localhost:8080
```

Without Docker (needs Rust stable and a Postgres 16 database):

```sh
export DATABASE_URL=postgres://postgres@localhost:5432/lms DEMO_MODE=true
cargo run --bin lms -- all
```

The home page lists the demo accounts when `DEMO_MODE=true`:

| Role | Sign in |
| --- | --- |
| Pupil, Year 6 | school `OAKHILL`, `amara.k`, `maple-river-seven` |
| Pupil, Year 10 | `OAKHILL`, `zainab.a`, `maple-river-seven` |
| Parent | `joyce.kofi@example.com` (the email code is shown on screen in demo mode) |
| Teacher, 6B | `rachel.hughes@oakhill.example`, `margin-demo-2026` |
| Teacher and safeguarding lead | `owen.clarke@oakhill.example`, `margin-demo-2026` |
| School admin, deputy lead | `sandra.patel@oakhill.example`, `margin-demo-2026` |
| System admin | `SYSADMIN_EMAIL` / `SYSADMIN_PASSWORD` |

## Commands

| Command | What it does |
| --- | --- |
| `lms web` | Migrates, loads the curriculum (and demo data), serves HTTP on `PORT` |
| `lms worker` | Consumes the outbox, rebuilds plans nightly, writes Monday class reviews, escalates safeguarding alerts |
| `lms all` | Both in one process (also the default; `LMS_ROLE` can set the role instead of an argument) |
| `lms migrate` | Runs migrations and exits |
| `lms seed` | Migrates and loads curriculum and demo data, then exits |

## Configuration

| Variable | Default | Notes |
| --- | --- | --- |
| `DATABASE_URL` | required | Postgres connection string |
| `PORT` | `8080` | |
| `DEMO_MODE` | off | Seeds two demo schools and shows demo details on screen. Turn off for real schools. |
| `SYSADMIN_EMAIL`, `SYSADMIN_PASSWORD` | unset | Creates or updates the platform admin at start-up (password 12+ characters) |
| `SECURE_COOKIES` | off | Set `true` behind HTTPS (Railway) |
| `LLM_API_KEY` | unset | Anthropic API key. Without it the helper runs offline and points pupils to the right step of the method. |
| `LLM_MODEL_FAST` | `claude-haiku-5-5` | Model for the lesson helper |
| `RUST_LOG` | `info` | |

## Layout

Domain-driven, hexagonal modular monolith. Bounded contexts are plain Rust crates with no web or database dependencies; `infra` implements their ports.

```
crates/
  kernel/          shared kernel: ids, roles, clock, domain events, errors
  identity/        usernames, passphrases, class-list import checks, school activation rule
  curriculum/      courses, concepts, prerequisite graph
  content/         lessons, flashcards, help ideas, content status lifecycle
  assessment/      items, marking, misconceptions, mocks, KS2 scaled-score guide
  learner/         mastery (BKT) and the 21-day review ladder
  planning/        study preferences and the weekly plan builder
  tutoring/        hint ladder, reflection feedback, helper prompt
  safeguarding/    risk screen, case lifecycle, escalation
  reporting/       pupil week, teacher weekly review
  notifications/   notifier port
  infra/           Postgres repositories, read models, outbox, auth, AI adapter, seed
  web/             Axum routes, Askama templates, HTMX partials, static assets
  worker/          outbox consumer and scheduled jobs
  app/             the `lms` binary
migrations/        SQL schema (one Postgres schema per context)
seeds/             curriculum.json and the script that builds it
```

Changes cross contexts through a transactional outbox (`platform.outbox`). The worker handles each event idempotently: an answer updates mastery and adds a fix task; a high-risk message opens a case, pauses the pupil's plan and alerts the safeguarding leads; closing a case can restart the plan.

## Deploy on Railway

The project `israel-lms-stack` has three services: `Postgres`, `web` and `worker`, both app services built from this repo's `Dockerfile`.

- `web`: start command `lms web`, healthcheck `/healthz`, variables `DATABASE_URL=${{Postgres.DATABASE_URL}}`, `SECURE_COOKIES=true`, `DEMO_MODE`, `SYSADMIN_EMAIL`, `SYSADMIN_PASSWORD`, optional `LLM_API_KEY`.
- `worker`: start command `lms worker`, `DATABASE_URL=${{Postgres.DATABASE_URL}}`.

Pushing to `main` redeploys both.

## What is built, and what isn't yet

Built: the five roles (pupil, parent, teacher, school admin, system admin) plus the safeguarding lead duty; pupil sign-in with school code and three-word passwords; parent sign-in by emailed code and invite-code linking; daily plan and week view with study preferences; Read, Review, Reflect lessons with hints, misconception feedback, reporting a question and the helper; spaced reviews and fixes; timed practice tests with a KS2 scaled-score guide; teacher class heatmap, needs-attention list, weekly review, pupil detail, password reset and setting work; school admin duties, activation, class-list import with login cards, parent invites; safeguarding alert list, acknowledge, record and close; system admin schools, content health, question reports, AI usage and job lag; 8 sample courses (26 topics, 171 questions).

Not yet: email and SMS go to the log (no provider wired up); staff two-factor sign-in; Postgres row-level security (tenancy is enforced in queries); AI course generation and the content review queue; the Python code runner; games and video; FSRS scheduling (a fixed ladder is used); picture passwords for Key Stage 1; GDPR export and deletion tools.
