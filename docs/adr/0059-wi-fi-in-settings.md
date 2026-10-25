# 0059. Wi-Fi in Settings

Status: **Accepted, built** (2026-10-25). CF-12. The owner: "All the above
let's get them all done."

## Context
- ADR 0058 put the pilot laptops' Wi-Fi firmware in the image. Settings had
  no Wi-Fi pane, so nobody could join a network, and the X1 Carbon Gen 9 has
  no Ethernet.
- CF-12 asks for a Wi-Fi pane in which IT's networks join by themselves and
  networks can be blocked.
- The base system has the drivers and `wpa_supplicant`, which keeps the
  networks a computer knows and joins them; rc.conf's `wlans_<radio>` and
  `ifconfig_wlan0="WPA DHCP"` run it on a station interface, with DHCP
  following the link.
- The VM can't show a join: QEMU emulates no Wi-Fi card, and the base
  system's simulated radio (`wtap(4)`) does only ad hoc and mesh, neither
  station nor access point.

## Decision
1. **glassd drives wpa_supplicant over its control socket**
   (`glass_platform::wifi::Control`), as `wpa_cli` does: one command per
   datagram.
   - Passwords go over the socket, never on a command line, where `ps`
     would show them.
   - Network names go hex-encoded, so no character in one can change a
     command.
   - Joined networks are saved to `/etc/wpa_supplicant.conf`, root's.
2. **The radio's station interface.** glassd gives the first radio it
   finds `wlan0` (`sysrc wlans_<radio>=wlan0`, `service netif start wlan0`).
   post-install sets `ifconfig_wlan0="WPA DHCP"` and the control socket in
   wpa_supplicant's config, so every start after that brings it up by
   itself.
3. **Three device settings**, set by IT, or on a computer nobody manages,
   by its owner:
   - `wifi.networks`: the organization's networks, by name. glassd keeps
     each in wpa_supplicant, tagged as the organization's, ahead of any
     network a person joined. A person can't forget one.
   - `wifi.blocked`: never joined. One the computer is on is left at once.
   - `wifi.only_organization`: people's own networks are turned off, and
     no other network can be joined.

   glassd applies them as it starts and whenever they change, and each
   change is on the audit record. The logic is in `glass-app-settings`
   (`wifi::reconcile`, `join`, `forget`) and is tested against a stand-in
   radio.
4. **The organization's passwords are never in policy or in Settings.**
   They live in `/usr/local/etc/glasshouse/wifi-secrets.toml`:
   `[networks."<name>"] password`, root's, mode 600, or refused, as model
   secrets are. IT or its tooling writes it. Policy names the networks
   only, so neither the person nor their agents ever see a password.
5. **The pane:**
   - the network in use, with its address;
   - the networks nearby, strongest first, with their signal and whether
     they need a password;
   - the known ones out of range;
   - why one can't be joined: blocked, only the organization's, or an
     802.1X sign-in, which Settings can't do yet (CF-10).
   - A password is asked for the first time, and again when wpa_supplicant
     says joining failed (`[TEMP-DISABLED]`, a wrong password).
   - The pane looks again every 8 seconds while open.
6. **The person's own, never an agent's.** `settings.wifi`,
   `settings.wifi_join` and `settings.wifi_forget` are Never for every
   agent and Auto for the Settings app (default policy). `wifi_join`
   carries a password, so it is one of the actions only a person takes
   (`PERSONS_OWN`) whatever an agent's rules say. The networks around a
   computer also say where it is.
   - Audit records name the network, never the password.
7. **How it's tested:**
   - parsing against wpa_supplicant's own reply formats;
   - the rules against a stand-in radio;
   - the pane in the apps' browser suite, against `glass-fake-wpa`: a
     control socket with networks in the air, which `make dev-host` runs
     too.
   - A real join is on the pilot laptop's checklist.

## Consequences
- The pilot's laptops join Wi-Fi from Settings, and IT's networks join by
  themselves.
- 802.1X (WPA2 Enterprise) networks show as not joinable until CF-10's
  sign-in exists.
- `wifi-secrets.toml` is written by hand or by IT's tooling until the
  console carries Wi-Fi passwords sealed to each computer. That needs the
  console to hold secrets per organization (EP-11).
- Some networks need a web sign-in page after joining (a captive portal).
  The browser shows that page. Nothing detects one yet.
