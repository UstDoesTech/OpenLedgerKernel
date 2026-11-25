[Unit]
Description=Read the API secret again, so a rotation reaches the running service
Wants=network-online.target
After=network-online.target

[Service]
Type=oneshot
TimeoutStartSec=300
StandardOutput=journal+console
StandardError=journal+console
# A secret that is missing an entry, or cannot be read, stops here and leaves the files as they
# are. The service reads its files again by itself when they change, so there is no restart.
ExecStart=/opt/ledger-api/fetch-secrets.sh ${secret_arguments}
