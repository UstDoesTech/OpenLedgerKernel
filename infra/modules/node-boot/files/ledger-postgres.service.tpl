[Unit]
Description=Ledger Postgres node
Wants=network-online.target
After=network-online.target docker.service ledger-data-disk.service
Requires=docker.service ledger-data-disk.service

[Service]
Restart=always
RestartSec=10
TimeoutStartSec=0
ExecStartPre=-/usr/bin/docker rm -f ${name}
ExecStartPre=/opt/ledger/fetch-tls.sh ${tls_arguments}
ExecStartPre=/opt/ledger/registry-login.sh ${login_arguments}
ExecStartPre=/usr/bin/docker pull ${image}
ExecStart=/usr/bin/docker run --name ${name} --read-only --cap-drop ALL --security-opt no-new-privileges --stop-timeout 60 --tmpfs /run/ledger:rw,mode=0700,uid=999,gid=999 --tmpfs /tmp:rw,mode=1777 --volume /var/lib/ledger/data:/var/lib/postgresql/data --volume /etc/ledger/node.json:/etc/ledger/node.json:ro --volume /etc/ledger/tls:/run/secrets/tls:ro --publish 5432:5432 ${image}
ExecStop=/usr/bin/docker stop ${name}

[Install]
WantedBy=multi-user.target
