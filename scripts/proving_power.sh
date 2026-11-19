#!/usr/bin/env bash
# List, pause or resume the proving environment's machines, to stop them costing money.
#
#   scripts/proving_power.sh status
#   scripts/proving_power.sh pause  [--scope cells|control-plane|all] [--yes]
#   scripts/proving_power.sh resume [--scope cells|control-plane|all] [--yes]
#
# A person runs this, from a shell signed in to the three clouds. An agent never does.
# Without --yes it only prints what it would do. It touches machines by name (the control
# plane's ledger-cp-m*, and the tenant's <tenant>-<cloud>-001-n*) and the tenant's managed
# databases, and nothing else. It deletes nothing.
#
# Paused machines still cost for their disks. The VPN gateways, tunnels and private endpoints
# cost whether or not a machine runs, and are the larger part of the bill: only destroying
# those layers stops them.
set -euo pipefail

TENANT_SLUG="${TENANT_SLUG:-bankofmumanddad}"
AWS_REGION="${AWS_REGION:-eu-north-1}"
CP_PREFIX="${CP_PREFIX:-ledger-cp-m}"

usage() {
  sed -n '2,15p' "$0" | sed 's/^# \{0,1\}//'
  exit "${1:-2}"
}

cmd="${1:-}"
[[ -n "$cmd" ]] || usage
shift || true
scope=all
yes=no
while [[ $# -gt 0 ]]; do
  case "$1" in
    --scope) scope="${2:-}"; shift 2 ;;
    --yes) yes=yes; shift ;;
    -h | --help) usage 0 ;;
    *) echo "unknown option: $1" >&2; usage ;;
  esac
done
case "$cmd" in status | pause | resume) ;; *) echo "unknown command: $cmd" >&2; usage ;; esac
case "$scope" in cells | control-plane | all) ;; *) echo "scope must be cells, control-plane or all" >&2; exit 2 ;; esac

prefixes=()
[[ "$scope" == cells || "$scope" == all ]] && prefixes+=("$TENANT_SLUG-")
[[ "$scope" == control-plane || "$scope" == all ]] && prefixes+=("$CP_PREFIX")

# Run a changing command only with --yes, otherwise print it.
act() {
  if [[ "$yes" == yes ]]; then
    echo "+ $*"
    "$@"
  else
    echo "would run: $*"
  fi
}

# --- AWS --------------------------------------------------------------------------------
aws_machines() { # prefix state
  aws ec2 describe-instances --region "$AWS_REGION" \
    --filters "Name=tag:Name,Values=$1*" "Name=instance-state-name,Values=$2" \
    --query 'Reservations[].Instances[].InstanceId' --output text
}

aws_power() {
  local from to verb
  if [[ "$cmd" == pause ]]; then from=running; verb=stop-instances; else from=stopped; verb=start-instances; fi
  for p in "${prefixes[@]}"; do
    # shellcheck disable=SC2207
    ids=($(aws_machines "$p" "$from"))
    ((${#ids[@]})) || { echo "AWS $p*: nothing to $cmd"; continue; }
    act aws ec2 "$verb" --region "$AWS_REGION" --instance-ids "${ids[@]}"
  done
  if [[ "$scope" != control-plane ]]; then
    local cluster="$TENANT_SLUG-aws-001" state
    state=$(aws rds describe-db-clusters --region "$AWS_REGION" --db-cluster-identifier "$cluster" \
      --query 'DBClusters[0].Status' --output text 2>/dev/null || true)
    if [[ "$cmd" == pause && "$state" == available ]]; then
      act aws rds stop-db-cluster --region "$AWS_REGION" --db-cluster-identifier "$cluster"
      echo "AWS stops a database for at most seven days, then starts it again by itself."
    elif [[ "$cmd" == resume && "$state" == stopped ]]; then
      act aws rds start-db-cluster --region "$AWS_REGION" --db-cluster-identifier "$cluster"
    fi
  fi
}

# --- Azure ------------------------------------------------------------------------------
azure_power() {
  local want
  if [[ "$cmd" == pause ]]; then want="VM running"; else want="VM deallocated"; fi
  for p in "${prefixes[@]}"; do
    while IFS=$'\t' read -r group name; do
      [[ -n "$name" ]] || continue
      if [[ "$cmd" == pause ]]; then
        act az vm deallocate --resource-group "$group" --name "$name" --no-wait
      else
        act az vm start --resource-group "$group" --name "$name" --no-wait
      fi
    done < <(az vm list -d --query "[?starts_with(name, '$p') && powerState=='$want'].[resourceGroup, name]" -o tsv)
  done
  if [[ "$scope" != control-plane ]]; then
    while IFS=$'\t' read -r group name ha; do
      [[ -n "$name" ]] || continue
      if [[ "$ha" != Disabled && "$ha" != None && -n "$ha" ]]; then
        echo "Azure $name has high availability on, and Azure cannot stop such a server. Only destroying it stops the cost."
        continue
      fi
      if [[ "$cmd" == pause ]]; then
        act az postgres flexible-server stop --resource-group "$group" --name "$name"
      else
        act az postgres flexible-server start --resource-group "$group" --name "$name"
      fi
    done < <(az postgres flexible-server list --query "[?starts_with(name, '$TENANT_SLUG-')].[resourceGroup, name, highAvailability.mode]" -o tsv)
  fi
}

# --- Google Cloud -----------------------------------------------------------------------
gcp_power() {
  local want verb
  if [[ "$cmd" == pause ]]; then want=RUNNING; verb=stop; else want=TERMINATED; verb=start; fi
  for p in "${prefixes[@]}"; do
    while read -r name zone; do
      [[ -n "$name" ]] || continue
      act gcloud compute instances "$verb" "$name" --zone "$zone" --quiet
    done < <(gcloud compute instances list --filter "name~^$p AND status=$want" --format 'value(name,zone.basename())')
  done
  if [[ "$scope" != control-plane ]]; then
    local instance="$TENANT_SLUG-gcp-001" policy
    policy=$(gcloud sql instances describe "$instance" --format 'value(settings.activationPolicy)' 2>/dev/null || true)
    if [[ "$cmd" == pause && "$policy" == ALWAYS ]]; then
      act gcloud sql instances patch "$instance" --activation-policy NEVER --quiet
    elif [[ "$cmd" == resume && "$policy" == NEVER ]]; then
      act gcloud sql instances patch "$instance" --activation-policy ALWAYS --quiet
    fi
  fi
}

# --- Status -----------------------------------------------------------------------------
status() {
  for p in "${prefixes[@]}"; do
    echo "== AWS $p*"
    aws ec2 describe-instances --region "$AWS_REGION" --filters "Name=tag:Name,Values=$p*" \
      --query 'Reservations[].Instances[].[Tags[?Key==`Name`]|[0].Value,State.Name]' --output text
    echo "== Azure $p*"
    az vm list -d --query "[?starts_with(name, '$p')].[name, powerState]" -o tsv
    echo "== Google Cloud $p*"
    gcloud compute instances list --filter "name~^$p" --format 'value(name,status)'
  done
}

if [[ "$cmd" == status ]]; then
  status
  exit 0
fi

[[ "$yes" == yes ]] || echo "Dry run: nothing changes without --yes."
aws_power
azure_power
gcp_power
if [[ "$cmd" == pause ]]; then
  echo "Paused machines still cost for their disks. The VPN gateways and private endpoints keep costing until their layers are destroyed."
fi
