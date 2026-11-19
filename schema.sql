-- Ledger schema. Applied identically to every node (one node = one region/cloud).
-- Everything lives in its own schema so the harness never touches other tables.

create schema if not exists ledger_lab;
set search_path to ledger_lab;

-- Projection only. `balance` is derived from `entry` locally on every node and is
-- never replicated as state. `next_seq` is only meaningful at the account's home.
create table if not exists account (
  id             text primary key,
  balance        bigint  not null default 0,
  next_seq       bigint  not null default 1,
  allow_negative boolean not null default false
);

-- One row per epoch, pushed from the control plane (consensus store) to every node.
-- Entries of `prev_epoch` with seq > prev_cutoff_seq are void, and so is any epoch
-- strictly between prev_epoch and epoch. This is what fences a stale home.
create table if not exists epoch_fence (
  account_id      text   not null references account(id),
  epoch           bigint not null,
  home            text   not null,
  prev_epoch      bigint not null default 0,
  prev_cutoff_seq bigint not null default 0,
  primary key (account_id, epoch)
);

-- Append-only log. Amounts are signed minor units: debits negative, credits positive.
create table if not exists entry (
  id           uuid primary key,
  transfer_id  uuid   not null,
  account_id   text   not null references account(id),
  epoch        bigint not null,
  seq          bigint not null,
  kind         text   not null check (kind in ('debit', 'credit')),
  amount       bigint not null,
  counterparty text,
  origin       text   not null,
  created_at   timestamptz not null default clock_timestamp(),
  unique (account_id, epoch, seq),
  unique (transfer_id, account_id, kind),
  check ((kind = 'debit' and amount < 0) or (kind = 'credit' and amount > 0))
);

-- Entries written by a stale home (or received from one). Never merged silently.
create table if not exists quarantine (
  id             uuid primary key,
  transfer_id    uuid   not null,
  account_id     text   not null,
  epoch          bigint not null,
  seq            bigint not null,
  kind           text   not null,
  amount         bigint not null,
  counterparty   text,
  origin         text   not null,
  created_at     timestamptz not null,
  reason         text   not null,
  quarantined_at timestamptz not null default now(),
  resolved       boolean not null default false
);

-- Local marker: both legs of this transfer are applied. Written at the debit's home.
create table if not exists settlement (
  transfer_id uuid primary key,
  settled_at  timestamptz not null default now()
);
