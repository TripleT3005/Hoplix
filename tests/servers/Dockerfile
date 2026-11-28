# Base

FROM debian:13

LABEL project="hoplix" \
      purpose="Hoplix integration test servers"

# Packages

RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        apache2 \
        ca-certificates \
        libpam-google-authenticator \
        openssh-server \
        openssl \
        python3 \
        python3-flask-cors \
        python3-moto \
        vsftpd \
    && rm -rf /var/lib/apt/lists/* \
    && rm -f /etc/ssh/ssh_host_*

# Users and folders

RUN useradd --create-home --shell /bin/bash demo \
    && mkdir -p /run/sshd /var/run/vsftpd/empty /srv/dav/digest /srv/dav/basic /var/lib/dav \
    && chown -R www-data:www-data /srv/dav /var/lib/dav \
    && a2enmod -q dav dav_fs auth_digest ssl \
    && a2dissite -q 000-default

# Configuration

COPY config/ /opt/hoplix/config/
COPY entrypoint.sh /opt/hoplix/entrypoint.sh
RUN chmod 755 /opt/hoplix/entrypoint.sh /opt/hoplix/config/pam-key-port.sh \
    && cp /opt/hoplix/config/pam-sshd /etc/pam.d/sshd \
    && cp /opt/hoplix/config/apache-ports.conf /etc/apache2/ports.conf \
    && cp /opt/hoplix/config/apache-hoplix.conf /etc/apache2/sites-available/hoplix.conf \
    && a2ensite -q hoplix

EXPOSE 2222 2223 2224 2221 2990 2280 2443 2900 40000-40019

VOLUME ["/state"]

HEALTHCHECK --interval=30s --timeout=5s --start-period=120s --start-interval=1s \
    CMD test -f /run/hoplix-ready

ENTRYPOINT ["/opt/hoplix/entrypoint.sh"]
