[Unit]
Description=Find, prepare and mount the data disk
After=local-fs.target
Before=ledger-postgres.service

[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=/opt/ledger/prepare-data-disk.sh ${storage_bytes} /var/lib/ledger/data

[Install]
WantedBy=multi-user.target
