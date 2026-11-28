#!/bin/sh
# Port check

case "$SSH_CONNECTION" in
    *" 2223") exit 0 ;;
    *) exit 1 ;;
esac
