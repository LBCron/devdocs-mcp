# Acme Quickstart

The Acme Widgets SDK ships first-class clients for TypeScript, Python, and Go.
Install it with your package manager of choice and export the API key before
the first call.

## Installation

Run the installer for your runtime and pin the minor version so upgrades stay
predictable in continuous integration.

```bash
npm install @acme/widgets@^2.1
export ACME_API_KEY="sk-test-not-a-real-key"
```

The installer writes a lockfile entry and prints the resolved version. A
project that already pins `@acme/widgets` keeps its resolution.

## First call

Create a client, then list the widgets visible to the key. The client reads
`ACME_API_KEY` from the environment, so no credentials land in source files.

```ts
import { Acme } from "@acme/widgets";

const acme = new Acme();
const widgets = await acme.widgets.list();
console.log(widgets.length);
```

The list call paginates by cursor. Pass `pageSize` to bound each response; the
default page holds twenty rows and the client follows `nextCursor` for you.

A longer paragraph follows so the packer must close a chunk near the target
size and start a fresh one. The Acme platform documents four widget kinds:
plain widgets, grouped widgets, widget collections, and widget aliases. Each
kind carries its own quota and its own retention window, and the platform
applies the strictest quota of the kinds a project mixes. Quota errors name
the kind that tripped, the window that applies, and the earliest instant the
window resets, so a backoff loop can schedule its retry without a second
round trip. The retention window interacts with export: a widget deleted
inside its window can still be exported by an admin, while a widget deleted
after the window is gone from every export surface including the audit log
mirror.
