#!/bin/sh
exec python3 -m http.server 4900 --bind 127.0.0.1 --directory "$(dirname "$0")"
