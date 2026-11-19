# Rotating the VPN pre-shared key

Do this when a key was shown somewhere it should not have been, for example the output of `az network vpn-connection shared-key show`, or on your own schedule. Do it before real traffic depends on the tunnels. Expect each affected tunnel to drop briefly while it re-establishes.

## What holds the keys

`TF_VAR_preshared_keys` is a secret: a JSON map of tunnel name to key, for the `interconnect` layer. Each key is 32 to 64 letters and digits, and no two are the same. The tunnel names are in `infra/envs/README.md`. Each key is also stored on the cloud side of its tunnel, which `interconnect` sets.

## Steps

1. Make new keys, one per tunnel:
   ```
   python3 -c "import secrets,string; a=string.ascii_letters+string.digits; print(''.join(secrets.choice(a) for _ in range(48)))"
   ```
   Do not paste them anywhere. Run the command once per tunnel.
2. Replace the `TF_VAR_preshared_keys` secret on `infra-plan` and on `infra-apply` with the new JSON map. The two must match.
3. Dispatch `infra-apply` with `plan` on `interconnect`. Read the plan in the plan store. It should change the connection resources, and only them.
4. Dispatch with `apply`, approve `infra-apply`, and do not cancel. VPN changes take a while.
5. Check each tunnel afterwards:
   ```
   az network vpn-connection show -g <resource group> -n <connection> --query connectionStatus -o tsv
   ```
   It should say `Connected`. Check the AWS and Google Cloud sides in their consoles.
6. Delete the old keys from wherever you kept them.

## Notes

- The plan identity needs the `VPN shared key reader` custom role to run step 3. See the [first run](first-run.md).
- Never run `az network vpn-connection shared-key show` where the output is saved or visible to others.
