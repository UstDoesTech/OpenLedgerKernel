[Unit]
Description=Apply the ledger schema to this cell's database, before the API starts
Wants=network-online.target
After=network-online.target docker.service
Requires=docker.service
Before=ledger-api.service

[Service]
Type=oneshot
RemainAfterExit=yes
Restart=on-failure
RestartSec=30
TimeoutStartSec=0
# Output goes to the serial console as for the API, so "Get system log" shows why it failed. The
# command prints one line: the schema's version and counts, never a host, a role or a password.
StandardOutput=journal+console
StandardError=journal+console
ExecStartPre=-/usr/bin/docker rm -f ${name}-migrate
ExecStartPre=/opt/ledger-api/fetch-secrets.sh ${secret_arguments}
ExecStartPre=/opt/ledger-api/registry-login.sh ${login_arguments}
ExecStartPre=/usr/bin/docker pull ${image}
# The same image as the API, run with the one word `migrate`. It gets the database's settings and
# nothing else: no listener, no tokens, no certificates.
ExecStart=/usr/bin/docker run --rm --name ${name}-migrate --read-only --cap-drop ALL --security-opt no-new-privileges --tmpfs /tmp:rw,mode=1777 --volume /etc/ledger-api/secrets:/run/secrets/api:ro ${environment_arguments} ${image} migrate

[Install]
WantedBy=multi-user.target
