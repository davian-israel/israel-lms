-- Margin LMS initial schema: one Postgres schema per bounded context.
-- Tenant tables carry school_id; every repository query filters by it.

CREATE SCHEMA IF NOT EXISTS identity;
CREATE SCHEMA IF NOT EXISTS curriculum;
CREATE SCHEMA IF NOT EXISTS content;
CREATE SCHEMA IF NOT EXISTS assessment;
CREATE SCHEMA IF NOT EXISTS learner;
CREATE SCHEMA IF NOT EXISTS planning;
CREATE SCHEMA IF NOT EXISTS tutoring;
CREATE SCHEMA IF NOT EXISTS safeguarding;
CREATE SCHEMA IF NOT EXISTS platform;

-- Identity & Tenancy ---------------------------------------------------------
CREATE TABLE identity.schools (
    id          uuid PRIMARY KEY,
    code        text NOT NULL UNIQUE,
    name        text NOT NULL,
    phase       text NOT NULL DEFAULT 'primary',
    active      boolean NOT NULL DEFAULT false,
    created_at  timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE identity.accounts (
    id             uuid PRIMARY KEY,
    first_name     text NOT NULL,
    last_name      text NOT NULL DEFAULT '',
    email          text UNIQUE,
    password_hash  text,
    phone          text,
    failed_logins  int NOT NULL DEFAULT 0,
    locked         boolean NOT NULL DEFAULT false,
    created_at     timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE identity.memberships (
    id          uuid PRIMARY KEY,
    account_id  uuid NOT NULL REFERENCES identity.accounts(id),
    school_id   uuid REFERENCES identity.schools(id),
    role        text NOT NULL CHECK (role IN ('student','parent','teacher','school_admin','system_admin')),
    duties      text[] NOT NULL DEFAULT '{}',
    username    text,
    year_group  int,
    active      boolean NOT NULL DEFAULT true,
    created_at  timestamptz NOT NULL DEFAULT now(),
    UNIQUE (school_id, username)
);
CREATE INDEX ON identity.memberships (account_id);
CREATE INDEX ON identity.memberships (school_id, role);

CREATE TABLE identity.classes (
    id          uuid PRIMARY KEY,
    school_id   uuid NOT NULL REFERENCES identity.schools(id),
    name        text NOT NULL,
    year_group  int NOT NULL,
    subject     text NOT NULL DEFAULT 'All subjects',
    UNIQUE (school_id, name)
);

CREATE TABLE identity.class_members (
    class_id       uuid NOT NULL REFERENCES identity.classes(id),
    membership_id  uuid NOT NULL REFERENCES identity.memberships(id),
    PRIMARY KEY (class_id, membership_id)
);

CREATE TABLE identity.parent_links (
    id                     uuid PRIMARY KEY,
    school_id              uuid NOT NULL REFERENCES identity.schools(id),
    student_membership_id  uuid NOT NULL REFERENCES identity.memberships(id),
    parent_membership_id   uuid REFERENCES identity.memberships(id),
    invite_code            text NOT NULL UNIQUE,
    relationship           text NOT NULL DEFAULT 'parent',
    verified_at            timestamptz
);

CREATE TABLE identity.sessions (
    token_hash     text PRIMARY KEY,
    membership_id  uuid NOT NULL REFERENCES identity.memberships(id),
    expires_at     timestamptz NOT NULL,
    created_at     timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE identity.login_codes (
    email       text NOT NULL,
    code_hash   text NOT NULL,
    expires_at  timestamptz NOT NULL,
    used        boolean NOT NULL DEFAULT false,
    created_at  timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX ON identity.login_codes (email, created_at);

-- Curriculum -----------------------------------------------------------------
CREATE TABLE curriculum.courses (
    id          uuid PRIMARY KEY,
    code        text NOT NULL UNIQUE,
    title       text NOT NULL,
    subject     text NOT NULL,
    key_stage   text NOT NULL,
    year_group  int NOT NULL,
    colour      text NOT NULL DEFAULT '#2D64C8',
    exam        text NOT NULL DEFAULT ''
);

CREATE TABLE curriculum.concepts (
    id         uuid PRIMARY KEY,
    course_id  uuid NOT NULL REFERENCES curriculum.courses(id),
    code       text NOT NULL UNIQUE,
    title      text NOT NULL,
    unit       text NOT NULL,
    position   int NOT NULL
);

CREATE TABLE curriculum.concept_edges (
    prerequisite_id  uuid NOT NULL REFERENCES curriculum.concepts(id),
    concept_id       uuid NOT NULL REFERENCES curriculum.concepts(id),
    PRIMARY KEY (prerequisite_id, concept_id)
);

-- Content --------------------------------------------------------------------
CREATE TABLE content.lessons (
    id              uuid PRIMARY KEY,
    concept_id      uuid NOT NULL UNIQUE REFERENCES curriculum.concepts(id),
    title           text NOT NULL,
    body            jsonb NOT NULL,
    status          text NOT NULL DEFAULT 'live',
    ai_generated    boolean NOT NULL DEFAULT false,
    version         int NOT NULL DEFAULT 1,
    updated_at      timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE content.flashcards (
    id          uuid PRIMARY KEY,
    concept_id  uuid NOT NULL REFERENCES curriculum.concepts(id),
    front       text NOT NULL,
    back        text NOT NULL
);

CREATE TABLE content.help_ideas (
    id          uuid PRIMARY KEY,
    concept_id  uuid NOT NULL REFERENCES curriculum.concepts(id),
    title       text NOT NULL,
    body        text NOT NULL
);

CREATE TABLE content.reports (
    id             uuid PRIMARY KEY,
    item_id        uuid,
    lesson_id      uuid,
    membership_id  uuid,
    note           text NOT NULL,
    status         text NOT NULL DEFAULT 'open',
    created_at     timestamptz NOT NULL DEFAULT now()
);

-- Assessment -----------------------------------------------------------------
CREATE TABLE assessment.items (
    id           uuid PRIMARY KEY,
    concept_id   uuid NOT NULL REFERENCES curriculum.concepts(id),
    kind         text NOT NULL,
    prompt       text NOT NULL,
    options      jsonb NOT NULL DEFAULT '[]',
    answer       text NOT NULL,
    explanation  text NOT NULL,
    hints        jsonb NOT NULL DEFAULT '[]',
    marks        int NOT NULL DEFAULT 1,
    usage        text NOT NULL DEFAULT 'quiz',  -- quiz | review | mock
    status       text NOT NULL DEFAULT 'live',
    position     int NOT NULL DEFAULT 0
);
CREATE INDEX ON assessment.items (concept_id, usage);

CREATE TABLE assessment.mocks (
    id         uuid PRIMARY KEY,
    course_id  uuid NOT NULL REFERENCES curriculum.courses(id),
    title      text NOT NULL,
    minutes    int NOT NULL,
    item_ids   uuid[] NOT NULL
);

CREATE TABLE assessment.attempts (
    id             uuid PRIMARY KEY,
    membership_id  uuid NOT NULL REFERENCES identity.memberships(id),
    kind           text NOT NULL,  -- lesson | review | mock
    concept_id     uuid,
    mock_id        uuid,
    started_at     timestamptz NOT NULL DEFAULT now(),
    finished_at    timestamptz,
    score          int,
    max_score      int
);
CREATE INDEX ON assessment.attempts (membership_id, started_at);

CREATE TABLE assessment.responses (
    id             uuid PRIMARY KEY,
    attempt_id     uuid NOT NULL REFERENCES assessment.attempts(id),
    item_id        uuid NOT NULL REFERENCES assessment.items(id),
    answer         text NOT NULL,
    correct        boolean NOT NULL,
    misconception  text,
    flagged        boolean NOT NULL DEFAULT false,
    created_at     timestamptz NOT NULL DEFAULT now(),
    UNIQUE (attempt_id, item_id)
);

-- Learner Model ----------------------------------------------------------------
CREATE TABLE learner.concept_states (
    membership_id  uuid NOT NULL REFERENCES identity.memberships(id),
    concept_id     uuid NOT NULL REFERENCES curriculum.concepts(id),
    mastery        double precision NOT NULL,
    stage          smallint NOT NULL DEFAULT 0,
    learned_on     date,
    due_on         date,
    secured_on     date,
    lapses         int NOT NULL DEFAULT 0,
    version        int NOT NULL DEFAULT 0,
    updated_at     timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (membership_id, concept_id)
);

CREATE TABLE learner.misconceptions (
    membership_id  uuid NOT NULL,
    concept_id     uuid NOT NULL,
    misconception  text NOT NULL,
    seen           int NOT NULL DEFAULT 1,
    fixed          boolean NOT NULL DEFAULT false,
    last_seen_at   timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (membership_id, concept_id, misconception)
);

-- Planning ---------------------------------------------------------------------
CREATE TABLE planning.enrolments (
    membership_id  uuid NOT NULL REFERENCES identity.memberships(id),
    course_id      uuid NOT NULL REFERENCES curriculum.courses(id),
    target_date    date,
    created_at     timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (membership_id, course_id)
);

CREATE TABLE planning.preferences (
    membership_id  uuid PRIMARY KEY REFERENCES identity.memberships(id),
    days           smallint[] NOT NULL DEFAULT '{1,2,3,4,5}',
    minutes        int NOT NULL DEFAULT 25,
    paused         boolean NOT NULL DEFAULT false,
    pause_reason   text
);

CREATE TABLE planning.tasks (
    id             uuid PRIMARY KEY,
    membership_id  uuid NOT NULL REFERENCES identity.memberships(id),
    on_date        date NOT NULL,
    kind           text NOT NULL,
    concept_id     uuid NOT NULL REFERENCES curriculum.concepts(id),
    minutes        int NOT NULL,
    position       int NOT NULL,
    done_at        timestamptz,
    created_at     timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX ON planning.tasks (membership_id, on_date);

-- Tutoring ---------------------------------------------------------------------
CREATE TABLE tutoring.messages (
    id             uuid PRIMARY KEY,
    membership_id  uuid NOT NULL REFERENCES identity.memberships(id),
    concept_id     uuid,
    author         text NOT NULL,  -- pupil | helper
    body           text NOT NULL,
    flagged        boolean NOT NULL DEFAULT false,
    created_at     timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE tutoring.reflections (
    id             uuid PRIMARY KEY,
    membership_id  uuid NOT NULL REFERENCES identity.memberships(id),
    concept_id     uuid NOT NULL,
    body           text NOT NULL,
    confidence     smallint NOT NULL,
    feedback       text NOT NULL,
    created_at     timestamptz NOT NULL DEFAULT now()
);

-- Safeguarding -------------------------------------------------------------------
CREATE TABLE safeguarding.cases (
    id               uuid PRIMARY KEY,
    school_id        uuid NOT NULL REFERENCES identity.schools(id),
    membership_id    uuid NOT NULL REFERENCES identity.memberships(id),
    severity         text NOT NULL,
    category         text NOT NULL,
    excerpt          text NOT NULL,
    status           text NOT NULL,
    opened_at        timestamptz NOT NULL,
    acknowledged_at  timestamptz,
    acknowledged_by  uuid,
    action           text,
    notes            text,
    closed_at        timestamptz,
    escalation_sent  smallint NOT NULL DEFAULT 1
);
CREATE INDEX ON safeguarding.cases (school_id, status);

-- Platform -----------------------------------------------------------------------
CREATE TABLE platform.outbox (
    id            bigserial PRIMARY KEY,
    event_type    text NOT NULL,
    payload       jsonb NOT NULL,
    created_at    timestamptz NOT NULL DEFAULT now(),
    processed_at  timestamptz,
    attempts      int NOT NULL DEFAULT 0,
    last_error    text
);
CREATE INDEX outbox_pending ON platform.outbox (id) WHERE processed_at IS NULL;

CREATE TABLE platform.audit_log (
    id          bigserial PRIMARY KEY,
    at          timestamptz NOT NULL DEFAULT now(),
    actor       uuid,
    school_id   uuid,
    action      text NOT NULL,
    detail      jsonb NOT NULL DEFAULT '{}'
);

CREATE TABLE platform.ai_usage (
    id             bigserial PRIMARY KEY,
    at             timestamptz NOT NULL DEFAULT now(),
    school_id      uuid,
    purpose        text NOT NULL,
    model          text NOT NULL,
    input_tokens   int NOT NULL DEFAULT 0,
    output_tokens  int NOT NULL DEFAULT 0
);

CREATE TABLE platform.weekly_reviews (
    id           uuid PRIMARY KEY,
    class_id     uuid NOT NULL REFERENCES identity.classes(id),
    week_start   date NOT NULL,
    body         text NOT NULL,
    created_at   timestamptz NOT NULL DEFAULT now(),
    UNIQUE (class_id, week_start)
);

CREATE TABLE platform.cron_runs (
    job       text NOT NULL,
    run_key   text NOT NULL,
    ran_at    timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (job, run_key)
);
