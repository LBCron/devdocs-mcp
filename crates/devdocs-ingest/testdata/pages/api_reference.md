# Acme API Reference

Every endpoint below lives under `https://api.acme.dev/v2` and speaks JSON
with UTF-8 encoding.

## Authentication

The API accepts bearer tokens only. Send the key in the `Authorization`
header; a key scoped to a project sees that project's widgets and nothing
else.

```http
Authorization: Bearer sk-test-not-a-real-key
```

### Key rotation

Rotation issues a second key that runs beside the first for one week. Both
keys stay valid during the overlap, so a deploy can cut over without a
maintenance window.

## Widgets

### List widgets

`GET /widgets` returns the project's widgets ordered by creation time.

```json
{ "widgets": [{ "id": "w_01", "kind": "plain" }], "nextCursor": null }
```

### Create a widget

`POST /widgets` accepts the `kind` field and returns the created row. A
duplicate name inside one project answers `409` with the existing row in the
body.

## A very large fenced block

The block below exceeds twice the chunk target on purpose: the segmenter must
keep it whole and never truncate it, because a truncated symbol table is
worse than an oversized chunk.

```
w_01  plain      created 2026-01-04  quota A  retention 30d
w_02  plain      created 2026-01-05  quota A  retention 30d
w_03  grouped    created 2026-01-09  quota B  retention 90d
w_04  grouped    created 2026-01-11  quota B  retention 90d
w_05  collection created 2026-02-01  quota C  retention 365d
w_06  collection created 2026-02-02  quota C  retention 365d
w_07  alias      created 2026-02-14  quota D  retention 7d
w_08  alias      created 2026-02-15  quota D  retention 7d
w_09  plain      created 2026-03-01  quota A  retention 30d
w_10  plain      created 2026-03-02  quota A  retention 30d
w_11  grouped    created 2026-03-19  quota B  retention 90d
w_12  grouped    created 2026-03-21  quota B  retention 90d
w_13  collection created 2026-04-01  quota C  retention 365d
w_14  collection created 2026-04-02  quota C  retention 365d
w_15  alias      created 2026-04-14  quota D  retention 7d
w_16  alias      created 2026-04-15  quota D  retention 7d
w_17  plain      created 2026-05-01  quota A  retention 30d
w_18  plain      created 2026-05-02  quota A  retention 30d
w_19  grouped    created 2026-05-19  quota B  retention 90d
w_20  grouped    created 2026-05-21  quota B  retention 90d
w_21  collection created 2026-06-01  quota C  retention 365d
w_22  collection created 2026-06-02  quota C  retention 365d
w_23  alias      created 2026-06-14  quota D  retention 7d
w_24  alias      created 2026-06-15  quota D  retention 7d
w_25  plain      created 2026-07-01  quota A  retention 30d
w_26  plain      created 2026-07-02  quota A  retention 30d
w_27  grouped    created 2026-07-19  quota B  retention 90d
w_28  grouped    created 2026-07-21  quota B  retention 90d
w_29  collection created 2026-08-01  quota C  retention 365d
w_30  collection created 2026-08-02  quota C  retention 365d
w_31  alias      created 2026-08-14  quota D  retention 7d
w_32  alias      created 2026-08-15  quota D  retention 7d
```
