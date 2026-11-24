# Rotate the API token

Replace the cluster's API bearer token on every box without downtime.

All boxes share one token. Rotation mints a new one on the leader and hands
it to every box; the old token keeps working until every box has the new
one.

**Discovery must be on.** The leader hands the token to the boxes it knows
through [discovery](../autodiscovery.md). With discovery off it knows none,
so a rotation changes only the leader's token and retires the old one
there at once. Turn discovery on first, or copy the new token to every box
by hand as in [A box that missed it](#a-box-that-missed-it).

## 1. Rotate

```bash
xdctl token rotate
```

It prints the new token. This is the only time any command shows it.

## 2. Watch it spread

```bash
xdctl token status
```

Each box shows one of:

| State | Meaning |
|---|---|
| `current` | Has the new token; the old one is retired there. |
| `overlap` | Accepts both. |
| `installing` | The leader is handing it the new token. |
| `stale` | Missed the rotation. Fix by hand, below. |
| `unreachable` | The leader cannot reach it. |

Once every box has used the new token, the old one is retired everywhere.

## 3. Update everything that holds the token

Each box rewrites `export XDEBBIE_TOKEN=` in its `/etc/xdebbie/xdebbie_env`
as it switches. Then:

- re-run `source /etc/xdebbie/xdebbie_env` in open shells;
- update the Secret capx reads (see
  [Run Kubernetes with Cluster API](../getting-started/cluster-api.md#3-give-capx-the-clusters-credentials)),
  and any other client that holds the token.

## A box that missed it

A box that was down for the whole rotation ends up `stale`. Give it the
current token by hand: copy `/etc/xdebbie/token` from the leader to the
same path on that box (root:root, mode 0400), then restart xdebbied there.

If the leader restarts mid-rotation, it resumes from
`/etc/xdebbie/token.previous`.
