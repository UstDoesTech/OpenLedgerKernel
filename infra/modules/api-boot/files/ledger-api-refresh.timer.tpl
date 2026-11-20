[Unit]
Description=Read the API secret again every few minutes

[Timer]
OnBootSec=${every_minutes}min
OnUnitActiveSec=${every_minutes}min
AccuracySec=15s

[Install]
WantedBy=timers.target
