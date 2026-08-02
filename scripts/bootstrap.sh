#!/usr/bin/env bash
# Idempotent bootstrap for the Oracle A1 Ubuntu 24.04 Minimal host.
# Phase 1 (default): user, hardening, podman, cockpit, tailscale, units, firewall for 25565.
# Phase 2 (--harden): restrict SSH and Cockpit to Tailscale. Run ONLY after:
#   1. tailscale up succeeded and you can SSH over the tailnet
#   2. key expiry is disabled for this node in the Tailscale admin console
# Manual Oracle console steps this script cannot do (see README):
#   VCN Security List ingress 25565/tcp from 0.0.0.0/0, reserved public IP.
set -euo pipefail

REPO_DIR=/opt/hijuepapuscraft
REPO_URL=https://github.com/juanloaiza21/hijuepapuscraft.git
ADMIN_USER="${ADMIN_USER:-papu}"
SSH_KEY="${SSH_KEY:-}"

need_root() { [[ $EUID -eq 0 ]] || { echo "run with sudo" >&2; exit 1; }; }

log() { echo ">>> $*"; }

ensure_pkg() {
  local missing=()
  for p in "$@"; do dpkg -s "$p" >/dev/null 2>&1 || missing+=("$p"); done
  if ((${#missing[@]})); then
    DEBIAN_FRONTEND=noninteractive apt-get install -y "${missing[@]}"
  fi
}

# Insert a rule before the first REJECT in a chain, only if absent.
# Published container ports traverse FORWARD (netavark DNAT), and OCI
# images pre-seed a FORWARD REJECT, so INPUT alone is not enough.
ensure_rule() {
  local chain=$1; shift
  if ! iptables -C "$chain" "$@" 2>/dev/null; then
    local pos
    pos=$(iptables -nL "$chain" --line-numbers | awk '/REJECT/{print $1; exit}')
    if [[ -n "$pos" ]]; then
      iptables -I "$chain" "$pos" "$@"
    else
      iptables -A "$chain" "$@"
    fi
  fi
}

persist_rules() {
  # netfilter-persistent save would capture netavark's live DNAT rules with
  # current container IPs; restored at boot they shadow the fresh rules and
  # kill the published port. Persist only non-container rules.
  iptables-save | grep -viE 'netavark|aardvark' > /etc/iptables/rules.v4
  ip6tables-save | grep -viE 'netavark|aardvark' > /etc/iptables/rules.v6 2>/dev/null || true
}

phase1() {
  log "timezone"
  timedatectl set-timezone America/Bogota

  log "apt update"
  apt-get update -y

  log "admin user ${ADMIN_USER}"
  if ! id "$ADMIN_USER" >/dev/null 2>&1; then
    adduser --disabled-password --gecos "" "$ADMIN_USER"
    usermod -aG sudo "$ADMIN_USER"
  fi
  if [[ -n "$SSH_KEY" ]]; then
    install -d -m 700 -o "$ADMIN_USER" -g "$ADMIN_USER" "/home/$ADMIN_USER/.ssh"
    grep -qxF "$SSH_KEY" "/home/$ADMIN_USER/.ssh/authorized_keys" 2>/dev/null \
      || echo "$SSH_KEY" >> "/home/$ADMIN_USER/.ssh/authorized_keys"
    chown "$ADMIN_USER:$ADMIN_USER" "/home/$ADMIN_USER/.ssh/authorized_keys"
    chmod 600 "/home/$ADMIN_USER/.ssh/authorized_keys"
  fi

  log "ssh hardening (key-only, no root)"
  # Guard: prevent SSH lockout if no key is provisioned.
  if [[ -z "$SSH_KEY" ]] && [[ ! -s "/home/$ADMIN_USER/.ssh/authorized_keys" ]]; then
    if [[ "${FORCE_SSH_HARDEN:-}" != "1" ]]; then
      echo "FATAL: SSH_KEY is empty and no authorized_keys for $ADMIN_USER." >&2
      echo "Hardening sshd now would strand the operator (no remote access after reboot)." >&2
      echo "Re-run with SSH_KEY set, or pre-provision /home/$ADMIN_USER/.ssh/authorized_keys manually." >&2
      echo "Note: OCI's ubuntu user SSH key remains active regardless (this drop-in does not remove it)." >&2
      echo "Override with: FORCE_SSH_HARDEN=1 $0" >&2
      exit 1
    fi
  fi
  install -m 644 /dev/stdin /etc/ssh/sshd_config.d/90-hijuepapus.conf <<'EOF'
PasswordAuthentication no
KbdInteractiveAuthentication no
PermitRootLogin no
EOF
  systemctl reload ssh || systemctl reload sshd

  log "packages: podman, firewall persistence, fail2ban, unattended-upgrades, git"
  ensure_pkg podman iptables-persistent netfilter-persistent fail2ban \
    unattended-upgrades git curl ca-certificates
  systemctl enable --now podman.socket

  log "firewall phase 1: open 25565 in INPUT and FORWARD, persist"
  ensure_rule INPUT -p tcp --dport 25565 -j ACCEPT
  ensure_rule FORWARD -p tcp --dport 25565 -j ACCEPT
  ensure_rule FORWARD -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT
  persist_rules

  log "cockpit from noble-backports (quadlet-aware cockpit-podman)"
  if ! grep -rq "noble-backports" /etc/apt/sources.list /etc/apt/sources.list.d/ 2>/dev/null; then
    echo "deb http://ports.ubuntu.com/ubuntu-ports noble-backports main universe" \
      > /etc/apt/sources.list.d/backports.list
    apt-get update -y
  fi
  DEBIAN_FRONTEND=noninteractive apt-get install -y -t noble-backports cockpit cockpit-podman
  systemctl enable --now cockpit.socket

  log "tailscale"
  if ! command -v tailscale >/dev/null 2>&1; then
    curl -fsSL https://tailscale.com/install.sh | sh
  fi

  log "unattended-upgrades on"
  install -m 644 /dev/stdin /etc/apt/apt.conf.d/20auto-upgrades <<'EOF'
APT::Periodic::Update-Package-Lists "1";
APT::Periodic::Unattended-Upgrade "1";
EOF

  log "repo at ${REPO_DIR}"
  if [[ -d "$REPO_DIR/.git" ]]; then
    if ! git -C "$REPO_DIR" pull --ff-only; then
      echo "ERROR: on-host repo clone has diverged (non-fast-forward). Resolve manually and re-run." >&2
      exit 1
    fi
  else
    git clone "$REPO_URL" "$REPO_DIR"
  fi
  [[ -f "$REPO_DIR/.env" ]] || {
    cp "$REPO_DIR/.env.example" "$REPO_DIR/.env"
    chmod 600 "$REPO_DIR/.env"
    log "EDIT ${REPO_DIR}/.env BEFORE STARTING ANYTHING"
  }
  ENV_FILE="$REPO_DIR/.env" "$REPO_DIR/scripts/gen-scoped-env.sh"

  log "install quadlet and systemd units"
  install -d /etc/containers/systemd
  ln -sf "$REPO_DIR"/containers/quadlet/*.network /etc/containers/systemd/
  ln -sf "$REPO_DIR"/containers/quadlet/*.container /etc/containers/systemd/
  for u in "$REPO_DIR"/containers/systemd/*; do
    ln -sf "$u" "/etc/systemd/system/$(basename "$u")"
  done
  systemctl daemon-reload
  systemctl enable mc.service mc-backup.timer mc-restart.timer \
    restic-forget.timer restic-check.timer

  log "phase 1 done. Next steps (see README first run walkthrough for full detail):"
  log "  1. tailscale up            (interactive auth)"
  log "  2. disable key expiry for this node in the Tailscale admin console"
  log "  3. edit ${REPO_DIR}/.env, then re-run scripts/gen-scoped-env.sh"
  log "  4. start mcnet-network.service and socket-proxy.service"
  log "  5. init the restic repo, then run the recreate scripts and start mc.service"
  log "  6. run the host validation gate, then start bot.service"
  log "  7. re-run with --harden once SSH over Tailscale works"
}

phase2() {
  log "firewall phase 2: SSH and Cockpit via Tailscale only"
  # Remove the OCI-seeded world-open SSH rule if present.
  iptables -D INPUT -p tcp -m state --state NEW -m tcp --dport 22 -j ACCEPT 2>/dev/null || true
  if iptables -C INPUT -p tcp -m state --state NEW -m tcp --dport 22 -j ACCEPT 2>/dev/null; then
    echo "WARNING: world-open SSH rule STILL PRESENT (rule text drift)." >&2
    echo "Offending rule:" >&2
    iptables -nL INPUT --line-numbers | grep -w 22 >&2 || true
    echo "Remove manually before trusting this hardening." >&2
  fi
  ensure_rule INPUT -i tailscale0 -p tcp --dport 22 -j ACCEPT
  ensure_rule INPUT -s 100.64.0.0/10 -p tcp --dport 22 -j ACCEPT
  ensure_rule INPUT -i tailscale0 -p tcp --dport 9090 -j ACCEPT
  persist_rules
  log "hardened. Verify a NEW ssh session over Tailscale before closing this one."
  log "Break-glass if locked out: OCI console serial connection (see RUNBOOK)."
}

need_root
case "${1:-}" in
  --harden) phase2 ;;
  "") phase1 ;;
  *) echo "usage: $0 [--harden]" >&2; exit 2 ;;
esac
