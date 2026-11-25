[Unit]
Description=Ledger API
Wants=network-online.target
After=network-online.target docker.service
Requires=docker.service

[Service]
Restart=always
RestartSec=10
TimeoutStartSec=0
# The machine has no way in: no SSH key and no public address. Its output goes to the serial
# console too, so "Get system log" in the cloud's console shows why a start failed. What the steps
# and the service print is one line each and never a key, a token or a password (a test holds that).
StandardOutput=journal+console
StandardError=journal+console
ExecStartPre=-/usr/bin/docker rm -f ${name}
ExecStartPre=/opt/ledger-api/fetch-secrets.sh ${secret_arguments}
ExecStartPre=/opt/ledger-api/registry-login.sh ${login_arguments}
ExecStartPre=/usr/bin/docker pull ${image}
ExecStart=/usr/bin/docker run --name ${name} --read-only --cap-drop ALL --security-opt no-new-privileges --stop-timeout 30 --tmpfs /tmp:rw,mode=1777 --volume /etc/ledger-api/secrets:/run/secrets/api:ro --publish ${public_port}:${public_port} --publish ${internal_port}:${internal_port} ${environment_arguments} ${image}
ExecStop=/usr/bin/docker stop ${name}

[Install]
WantedBy=multi-user.target
