# Running a Server Other Machines Reach

Everything else in these docs assumes the server is on `127.0.0.1`, where the
machine is the boundary. This page is about the day that stops being true: a
homelab box, a VPS, a team of three pointing their editors at one memory.

Read it before you bind anything but loopback. The server holds every prompt
anybody typed, every path they opened, and every summary written from them.

---

## What changes off loopback

| on `127.0.0.1` | anywhere else |
|---|---|
| The port is the boundary | The network is the boundary |
| No token needed | A token is required, and `serve` refuses to start without one |
| Plain HTTP is fine | Plain HTTP puts the tokens *and* the memory on the wire |
| One person's memory | Several people's, sharing pages and not sharing handoffs |
| "Who changed this" is you | `anamnesis audit` is the only answer |

`anamnesis serve` already refuses to bind a non-loopback address with no token
configured. That refusal is the last thing standing between a laptop on a café
network and a readable history of everything you have worked on, so
`--allow-anonymous` is for exactly one case: something in front of it is
already doing the authenticating.

---

## 1. Give each person a token

```bash
anamnesis token --operator alice     # prints alice=anam_...
anamnesis token --operator bob
```

The server accepts a set; each machine presents one:

```bash
# on the server
export ANAMNESIS_TOKENS='alice=anam_...,bob=anam_...'

# on alice's machine, before launching the agent
export ANAMNESIS_TOKEN=anam_...
```

Naming the operator is what makes the rest of this page work: sessions record
who ran them, `[slots] per_user` can keep handoffs personal, and every line in
the audit log has somebody's name on it instead of "someone unnamed".

**Rotating one.** Mint a new secret, add it to `ANAMNESIS_TOKENS` alongside the
old one, restart, let that person switch, then remove the old one and restart
again. The overlap is what keeps a rotation from being an outage: a hook whose
token stopped working queues its events and says so, but nothing it queued
reaches memory until somebody notices.

---

## 2. Put TLS in front of it

Anamnesis speaks HTTP. It does not terminate TLS and will not learn to: a
memory server is not the right place to keep a certificate renewal working.

Bind it to loopback on the server machine and let a reverse proxy hold the
public address.

**Caddy**, which gets a certificate on its own:

```caddyfile
memory.example.com {
	reverse_proxy 127.0.0.1:8080
}
```

**nginx**, with a certificate you already have:

```nginx
server {
	listen 443 ssl;
	server_name memory.example.com;

	ssl_certificate     /etc/letsencrypt/live/memory.example.com/fullchain.pem;
	ssl_certificate_key /etc/letsencrypt/live/memory.example.com/privkey.pem;

	location / {
		proxy_pass http://127.0.0.1:8080;
		proxy_set_header Host $host;

		# A four-megabyte tool response is an ordinary event. nginx's default
		# body limit is 1 MB, and the events it refuses are the ones a hook
		# then keeps in its queue for a server that will never take them.
		client_max_body_size 32m;
	}
}
```

With the proxy holding the address, the server itself stays on loopback:

```bash
export ANAMNESIS_TOKENS='alice=anam_...,bob=anam_...'
anamnesis serve                      # 127.0.0.1:8080, behind the proxy
```

Then each machine points at the public name:

```bash
export ANAMNESIS_SERVER=https://memory.example.com
```

If you bind `0.0.0.0` directly instead, the tokens travel in clear text on
every hook of every session. Do not.

---

## 3. Decide what the browser can see

`/ui` and `/api/v1` are the two parts of the surface that can read the whole of
memory — the hook and handoff endpoints take an event and hand back one note,
and neither returns an arbitrary page.

Both sit behind the same tokens. The browser additionally accepts a token as an
HTTP Basic password, because a browser will not attach a bearer token to a link
somebody clicked. If nobody needs to browse:

```bash
anamnesis serve --no-ui
```

The JSON API stays, which is what a dashboard should be built on anyway.

---

## 4. Separate what should be separate

**Handoffs.** By default a project keeps one pending handoff and whoever starts
next is handed it. On one machine that is right; on a shared server it is a
silent theft. In the project's `.anamnesis.toml`:

```toml
[slots]
per_user = true
```

Now the note a session leaves waits for the operator whose token that session
arrived under. Pages stay shared — that is the point of a shared memory.

**Projects.** One server holds many. Identity comes from the git remote, so two
clones of the same repository share one memory whether or not that was
intended. `[scope] project` in the marker file pins it deliberately.

---

## 5. Know what happened

```bash
anamnesis audit --everywhere        # every deliberate change, newest first
```

Capture is recorded as sessions; this is the log of changes people made — pages
written or forgotten, sessions removed, handoffs claimed, proposals carried
out. On a shared server it is the difference between "why does this page say
that now" having an answer and having a guess.

It is also readable over the API:

```
GET /api/v1/scopes/{workspace}/{project}/audit
```

---

## 6. Back it up somewhere else

The wiki is a git repository, so it can be pushed. The transcripts under `raw/`
cannot be rebuilt from anything:

```bash
anamnesis backup --out /backups/anamnesis-$(date +%F).tar.gz
```

The index is copied through SQLite's own backup API, so this is safe to run
while the server is recording. Keep the archives off the machine that made
them; a backup on the disk that failed is not one.

---

## Before you open the port

- [ ] `ANAMNESIS_TOKENS` set, one entry per person, each with a name
- [ ] TLS terminated by something in front; the server itself on loopback
- [ ] Proxy body limit raised past 1 MB (32 MB is comfortable)
- [ ] `--no-ui` decided one way or the other
- [ ] `[slots] per_user = true` where handoffs should be personal
- [ ] A backup that runs on a schedule and lands off the machine
- [ ] `anamnesis status` from another machine says `Auth: required — this
      client is <name>`
- [ ] `curl https://memory.example.com/health` answers `ok`, and
      `curl https://memory.example.com/api/v1/scopes` **without** a token
      answers `401`

That last pair is the whole test: the first says the server is alive, and the
second says it is not readable by everyone who can reach it.
