[Unit]
Description=etcd member
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
ExecStart=/usr/bin/docker run --name ${name} --user 999:999 --read-only --cap-drop ALL --security-opt no-new-privileges --stop-timeout 30 --tmpfs /tmp:rw,mode=1777 --volume /var/lib/ledger/data:/var/lib/etcd --volume /etc/ledger/tls:/etc/etcd/tls:ro --publish 2379:2379 --publish 2380:2380 ${image} etcd ${etcd_arguments}
ExecStop=/usr/bin/docker stop ${name}

[Install]
WantedBy=multi-user.target
