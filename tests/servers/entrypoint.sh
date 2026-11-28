#!/bin/bash
set -euo pipefail

# Paths

state=/state
server="$state/server"
host_state="${HOPLIX_HOST_STATE:-$state}"
user=demo

# Generation

random_hex() {
    openssl rand -hex "$1"
}

generate() {
    echo "generating new credentials in the state folder"
    umask 077
    mkdir -p "$server"

    local secret
    secret=$(python3 -c 'import base64, os; print(base64.b32encode(os.urandom(20)).decode())')
    cat > "$server/secrets.env" <<EOF
PASSWORD=$(random_hex 16)
PASSPHRASE=$(random_hex 16)
TOTP=$secret
S3_ACCESS_KEY=HOPLIX$(random_hex 7 | tr 'a-f' 'A-F')
S3_SECRET=$(random_hex 20)
EOF
    . "$server/secrets.env"

    ssh-keygen -q -t ed25519 -N '' -C hoplix-test-servers -f "$server/ssh_host_ed25519_key"
    ssh-keygen -q -t ed25519 -N '' -C other -f "$server/other_key"
    cut -d ' ' -f 1,2 "$server/other_key.pub" > "$server/other_key.txt"
    rm -f "$server/other_key" "$server/other_key.pub"

    ssh-keygen -q -t ed25519 -N "$PASSPHRASE" -C hoplix-test-sftp -f "$state/id_ed25519_passphrase"
    ssh-keygen -q -t ed25519 -N '' -C hoplix-test-ki -f "$state/id_ed25519"
    cat "$state/id_ed25519_passphrase.pub" "$state/id_ed25519.pub" > "$server/authorized_keys"

    openssl req -x509 -newkey rsa:3072 -nodes -days 3650 -subj "/CN=localhost" \
        -addext "subjectAltName=DNS:localhost,IP:127.0.0.1" \
        -addext "basicConstraints=critical,CA:FALSE" \
        -addext "keyUsage=critical,digitalSignature,keyEncipherment" \
        -addext "extendedKeyUsage=serverAuth" \
        -keyout "$server/tls-key.pem" -out "$state/certificate.pem" 2>/dev/null
}

# Installation

install_credentials() {
    . "$server/secrets.env"

    install -m 600 "$server/ssh_host_ed25519_key" /etc/ssh/ssh_host_ed25519_key
    ssh-keygen -y -f /etc/ssh/ssh_host_ed25519_key > /etc/ssh/ssh_host_ed25519_key.pub

    echo "$user:$PASSWORD" | chpasswd
    install -d -m 700 -o "$user" -g "$user" "/home/$user/.ssh"
    install -m 600 -o "$user" -g "$user" "$server/authorized_keys" "/home/$user/.ssh/authorized_keys"

    install -d -m 700 /etc/hoplix/totp
    printf '%s\n" TOTP_AUTH\n' "$TOTP" > "/etc/hoplix/totp/$user"
    chmod 600 "/etc/hoplix/totp/$user"

    install -d -m 755 /etc/hoplix/tls
    install -m 600 "$server/tls-key.pem" /etc/hoplix/tls/key.pem
    install -m 644 "$state/certificate.pem" /etc/hoplix/tls/cert.pem

    install -d -m 750 -g www-data /etc/hoplix/dav
    local digest
    digest=$(printf '%s:Hoplix:%s' "$user" "$PASSWORD" | md5sum | cut -d ' ' -f 1)
    echo "$user:Hoplix:$digest" > /etc/hoplix/dav/digest
    printf '%s' "$PASSWORD" | htpasswd -i -c -B /etc/hoplix/dav/basic "$user" 2>/dev/null
    chgrp www-data /etc/hoplix/dav/digest /etc/hoplix/dav/basic
    chmod 640 /etc/hoplix/dav/digest /etc/hoplix/dav/basic
}

# Client settings

write_environment() {
    local tls_fingerprint ssh_fingerprint other_key
    tls_fingerprint="SHA256:$(openssl x509 -in "$state/certificate.pem" -noout -fingerprint -sha256 | cut -d '=' -f 2)"
    ssh_fingerprint=$(ssh-keygen -l -f /etc/ssh/ssh_host_ed25519_key.pub | cut -d ' ' -f 2)
    other_key=$(cat "$server/other_key.txt")
    printf '%s\n' "$tls_fingerprint" "$ssh_fingerprint" > "$state/fingerprints.txt"
    umask 077
    cat > "$state/test.env" <<EOF
HOPLIX_TEST_SFTP_HOST=127.0.0.1
HOPLIX_TEST_SFTP_PORT=2222
HOPLIX_TEST_SFTP_USER=$user
HOPLIX_TEST_SFTP_PASSWORD=$PASSWORD
HOPLIX_TEST_SFTP_KEY=$host_state/id_ed25519_passphrase
HOPLIX_TEST_SFTP_PASSPHRASE=$PASSPHRASE
HOPLIX_TEST_SFTP_OTHER_KEY=$other_key
HOPLIX_TEST_SFTP_ROOT=/home/$user
HOPLIX_TEST_SSH_FINGERPRINT=$ssh_fingerprint
HOPLIX_TEST_KI_HOST=127.0.0.1
HOPLIX_TEST_KI_USER=$user
HOPLIX_TEST_KI_KEY=$host_state/id_ed25519
HOPLIX_TEST_KI_PASSWORD=$PASSWORD
HOPLIX_TEST_KI_TOTP=$TOTP
HOPLIX_TEST_KI_KEY_PORT=2223
HOPLIX_TEST_KI_PASSWORD_PORT=2224
HOPLIX_TEST_FTP_HOST=127.0.0.1
HOPLIX_TEST_FTP_PORT=2221
HOPLIX_TEST_FTPS_PORT=2990
HOPLIX_TEST_FTP_PROTOCOL=ftp
HOPLIX_TEST_FTP_USER=$user
HOPLIX_TEST_FTP_PASSWORD=$PASSWORD
HOPLIX_TEST_FTP_ROOT=/home/$user
HOPLIX_TEST_DAV_HOST=127.0.0.1
HOPLIX_TEST_DAV_PORT=2280
HOPLIX_TEST_DAVS_PORT=2443
HOPLIX_TEST_DAV_USER=$user
HOPLIX_TEST_DAV_PASSWORD=$PASSWORD
HOPLIX_TEST_DAV_DIGEST_PATH=/digest
HOPLIX_TEST_DAV_BASIC_PATH=/basic
HOPLIX_TEST_S3_ENDPOINT=http://127.0.0.1:2900
HOPLIX_TEST_S3_REGION=us-east-1
HOPLIX_TEST_S3_BUCKET=demo
HOPLIX_TEST_S3_ACCESS_KEY=$S3_ACCESS_KEY
HOPLIX_TEST_S3_SECRET=$S3_SECRET
HOPLIX_TEST_TLS_CERTIFICATE=$host_state/certificate.pem
HOPLIX_TEST_TLS_FINGERPRINT=$tls_fingerprint
EOF
}

# State

rm -f /run/hoplix-ready
if [ -z "$(ls -A "$state")" ]; then
    generate
elif [ ! -f "$server/secrets.env" ]; then
    echo "the state folder is not empty but has no credentials; empty it to regenerate them" >&2
    exit 1
fi
install_credentials
write_environment
unset PASSWORD PASSPHRASE TOTP S3_ACCESS_KEY S3_SECRET

# Servers

pids=()

run() {
    "$@" &
    pids+=("$!")
}

stop() {
    kill -TERM "${pids[@]}" 2>/dev/null || true
    wait || true
}

trap 'stop; exit 0' TERM INT

rm -f /var/run/apache2/apache2.pid
run /usr/sbin/sshd -D -e -f /opt/hoplix/config/sshd-sftp.conf
run /usr/sbin/sshd -D -e -f /opt/hoplix/config/sshd-ki-key.conf
run /usr/sbin/sshd -D -e -f /opt/hoplix/config/sshd-ki-password.conf
run /usr/sbin/vsftpd /opt/hoplix/config/vsftpd-explicit.conf
run /usr/sbin/vsftpd /opt/hoplix/config/vsftpd-implicit.conf
run bash -c '. /etc/apache2/envvars && mkdir -p "$APACHE_RUN_DIR" "$APACHE_LOCK_DIR" "$APACHE_LOG_DIR" && exec /usr/sbin/apache2 -D FOREGROUND'
run moto_server -H 0.0.0.0 -p 2900

for _ in $(seq 1 150); do
    if python3 -c 'import urllib.request; urllib.request.urlopen(urllib.request.Request("http://127.0.0.1:2900/demo", method="PUT"))' 2>/dev/null; then
        echo "S3 bucket demo is ready"
        break
    fi
    sleep 0.2
done

touch /run/hoplix-ready
echo "Hoplix test servers are running"
set +e
wait -n
echo "a server stopped unexpectedly, shutting down" >&2
stop
exit 1
