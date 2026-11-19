# Local environment and checks. See compose.yaml for the ports.
COMPOSE ?= docker compose

.PHONY: up down ps logs reset-faults test-integration replay check image test-image

up:  ## three Postgres nodes, etcd and Toxiproxy, healthy before returning
	$(COMPOSE) up -d --wait

down:  ## remove containers and volumes
	$(COMPOSE) down -v --remove-orphans

ps:
	$(COMPOSE) ps

logs:
	$(COMPOSE) logs --no-color --tail=200

reset-faults:  ## remove every toxic and re-enable every proxy
	curl -fsS -X POST http://127.0.0.1:8474/reset

test-integration: up  ## Rust tests that need the environment
	cargo test -p conformance --features etcd -- --ignored

replay:  ## repeat one randomised run: make replay SEED=123 (needs `make up`)
	@test -n "$(SEED)" || { echo "usage: make replay SEED=<seed>"; exit 2; }
	LEDGER_SEED=$(SEED) cargo test -p conformance --features etcd --test randomised -- --ignored --nocapture

check:  ## the checks CONTRIBUTING.md asks for, without the environment
	cargo fmt --check
	cargo clippy --all-targets -- -D warnings
	cargo clippy -p ledgerctl --all-targets --features failover -- -D warnings
	cargo clippy -p conformance --all-targets --features etcd -- -D warnings
	cargo test
	cargo test -p ledgerctl --features failover
	pytest -q

image:  ## build the Postgres node image
	docker build -f images/postgres/Dockerfile -t ledger-postgres:local .

test-image: image  ## three real containers against the image
	LEDGER_IMAGE=ledger-postgres:local cargo test -p node-agent --test containers -- --ignored --nocapture
