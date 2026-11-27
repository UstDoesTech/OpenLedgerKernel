[Unit]
Description=etcd member
Wants=network-online.target
After=network-online.target docker.service ledger-data-disk.service
Requires=docker.service ledger-data-disk.service

[Service]
Restart=always
RestartSec=10
TimeoutStartSec=0
# The machine has no way in: no SSH key and no public address. Output goes to the serial console as
# well, so "Get system log" in the cloud's console shows why a start failed: the certificate fetch,
# the registry sign-in and the image pull each print one line, and etcd's own log follows. None of
# them prints a key or a certificate (a test holds that).
StandardOutput=journal+console
StandardError=journal+console
ExecStartPre=-/usr/bin/docker rm -f ${name}
ExecStartPre=/opt/ledger/fetch-tls.sh ${tls_arguments}
ExecStartPre=/opt/ledger/registry-login.sh ${login_arguments}
ExecStartPre=/usr/bin/docker pull ${image}
ExecStart=/usr/bin/docker run --name ${name} --user 999:999 --read-only --cap-drop ALL --security-opt no-new-privileges --stop-timeout 30 --tmpfs /tmp:rw,mode=1777 --volume /var/lib/ledger/data:/var/lib/etcd --volume /etc/ledger/tls:/etc/etcd/tls:ro --publish 2379:2379 --publish 2380:2380 ${image} etcd ${etcd_arguments}
ExecStop=/usr/bin/docker stop ${name}
# After every stop, one fixed line: the exit code, whether the kernel killed it for memory, and
# Docker's own error. It is "no such object" when the start never got as far as the container.
ExecStopPost=-/bin/sh -c '/usr/bin/docker inspect --format "etcd-member stopped: code {{.State.ExitCode}}, out of memory {{.State.OOMKilled}}, error [{{.State.Error}}]" ${name} > /dev/console 2>&1'

[Install]
WantedBy=multi-user.target
