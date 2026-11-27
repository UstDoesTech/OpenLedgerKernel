[Unit]
Description=Make this managed cell's application role, before the schema step and the API start
Wants=network-online.target
After=network-online.target docker.service
Requires=docker.service
Before=ledger-api-migrate.service ledger-api.service
# Once it has worked it is skipped on every later start of this machine, which counts as success for
# the units that wait for it. The machine's role loses the right to read the administrator's secret
# when the setting is switched off, so a second run could only fail, and the API must not wait on that.
ConditionPathExists=!/etc/ledger-api/application-role-made

[Service]
Type=oneshot
RemainAfterExit=yes
Restart=on-failure
RestartSec=30
TimeoutStartSec=0
# Output goes to the serial console as for the API, so "Get system log" shows why it failed. The
# command prints one line of counts, never a host, a role or a password.
StandardOutput=journal+console
StandardError=journal+console
ExecStartPre=-/usr/bin/docker rm -f ${name}-role
ExecStartPre=/opt/ledger-api/fetch-secrets.sh ${secret_arguments}
ExecStartPre=/opt/ledger-api/registry-login.sh ${login_arguments}
ExecStartPre=/usr/bin/docker pull ${image}
# The administrator's connection goes to a file in memory that only this unit's container mounts. The
# API's container never does, and the file is not in the API's secret.
ExecStartPre=/opt/ledger-api/fetch-administrator.sh ${administrator_secret}
ExecStart=/usr/bin/docker run --rm --name ${name}-role --read-only --cap-drop ALL --security-opt no-new-privileges --tmpfs /tmp:rw,mode=1777 --volume /etc/ledger-api/secrets:/run/secrets/api:ro --volume /run/ledger-administrator:/run/administrator:ro ${environment_arguments} ${image} managed-role
# Gone as soon as the step ends, whether it worked or not.
ExecStartPost=/usr/bin/rm -rf /run/ledger-administrator
ExecStartPost=/usr/bin/touch /etc/ledger-api/application-role-made
ExecStopPost=/usr/bin/rm -rf /run/ledger-administrator

[Install]
WantedBy=multi-user.target
