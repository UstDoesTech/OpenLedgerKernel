[Unit]
Description=Find, prepare and mount the data disk
After=local-fs.target
Before=etcd-member.service

[Service]
Type=oneshot
RemainAfterExit=yes
StandardOutput=journal+console
StandardError=journal+console
ExecStart=/opt/ledger/prepare-data-disk.sh ${storage_bytes} /var/lib/ledger/data

[Install]
WantedBy=multi-user.target
