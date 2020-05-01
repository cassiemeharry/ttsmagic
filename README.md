# ttsmagic.cards

Copyright Cassie Meharry <cassie@prophetessof.tech>

This builds an executable, `ttsmagic-server`, that serves a webapp that converts
Magic: the Gathering decks listed online elsewhere into Tabletop Simulator
decks.

## Building

The `shell.nix` environment includes a Rust installation that's capable of
building both the server and frontend. `cargo build` and `cargo run` should both
work, though the latter may require a few environment variables set before
anything useful happens.

## Deployment.

This application is currently deployed with Docker. There is a Nix derivation in
`nix/docker-image.nix` that builds a minimal container for the application. The
`Makefile` includes a `deploy` target that builds that derivation and uploads it
to the production site. You can run `make` overriding the `DEPLOY_HOST` variable
to change the server it deploys to.

The application requires a TOML-formatted file containing two secrets labeled
`steam_api_key` and `session_private_key_hex`. If it's not explicitly named
(either as an argument or environment variable), the app will load
`secrets.toml` from the app root.

The application looks for the following environment variables:

| Environment Variable | Required |
| --- | --- |
| `DB_HOST` | no |
| `DB_PORT` | no |
| `DB_NAME` | no |
| `DB_USER` | no |
| `DB_PASSWORD` | **yes** |
| `HOST` | no |
| `REDIS_HOST` | **yes** |
| `REDIS_PORT` | no |
| `REDIS_USER` | no |
| `REDIS_PASSWORD` | no |
| `SECRETS_TOML` | no |
| `SENTRY_DSN` | no |
| `WEB_PORT` | no |
| `WS_PORT` | no |

### nginx proxy

The `server` command expects HTTP to be served at the root directory, and the WS
port to be served at `/ws/`. This can be achieved with nginx like this:

```nginx
server {
    location / {
        proxy_pass http://127.0.0.1:$WEB_PORT/;
        proxy_http_version 1.1;
        proxy_read_timeout 300s;
        proxy_set_header Host $host;
        proxy_set_header X-Forwarded-For $remote_addr;
    }

    location /ws/ {
        proxy_pass http://127.0.0.1:$WS_PORT/;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "Upgrade";
        proxy_set_header Host $host;
        proxy_set_header X-Forwarded-For $remote_addr;
    }
}
```

…with `$WEB_PORT` and `$WS_PORT` replaced with values the correct ports to
connect to the app server.
