# Folders

Alias /digest /srv/dav/digest
Alias /basic /srv/dav/basic

# Digest

<Directory /srv/dav/digest>
    Dav On
    Options Indexes
    AuthType Digest
    AuthName "Hoplix"
    AuthDigestProvider file
    AuthUserFile /etc/hoplix/dav/digest
    Require valid-user
</Directory>

# Basic

<Directory /srv/dav/basic>
    Dav On
    Options Indexes
    AuthType Basic
    AuthName "Hoplix"
    AuthBasicProvider file
    AuthUserFile /etc/hoplix/dav/basic
    Require valid-user
</Directory>
