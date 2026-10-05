---
name: ais-tracing
description: Help someone use AIS Tracing, the desktop app that follows one value (a correlation id, an order reference) through every container of an Azure Cosmos DB account and draws where it got to on a timeline. Use this whenever the user mentions AIS Tracing or ais-tracing, or is tracing a correlation id across Cosmos DB containers and asks about the correlation key, lanes that stay empty, a value that can't be found, a 403 when scanning, Cosmos DB data-plane roles (Built-in Data Reader), an expired Azure session, or error rules. Also use it when they want to report an AIS Tracing bug or ask the author for help.
---

# AIS Tracing

AIS Tracing follows one value through a Cosmos DB account. It samples the
account's containers, works out which field ties the steps of one flow
together, then, for a value the user pastes, draws each container (or each
step) as a lane on a timeline. Empty lanes matter as much as full ones: they
show where the data hasn't arrived.

It has no login of its own: it uses the Azure CLI session (`az login`) and the
signed-in user's access to the account. The person you are helping is
usually a developer or someone on support looking at a real, often client's,
system.

## How it works

- **Pick an account.** The welcome screen lists the Cosmos DB accounts in all
  the user's subscriptions. **Forget this account** clears it.
- **Setup ⚙.** From a sample of every container, the app proposes:
  - the **correlation key** ("links the steps"): the field whose value is the
    same across the steps of one flow. It's decided from the data, not from
    field names, so `orderId` in one container and `entityId` in another are
    recognised as the same key when they carry the same values;
  - the **order by** field ("sequences the steps"), usually a timestamp;
  - the **lane**: a Cosmos container by default, or a field such as a step or
    workflow name;
  - **error rules** ("means failure when it equals"): cards matching any of
    them are drawn in red. What counts as a failure is the user's call.
  Every proposal is a dropdown the user can change.
- **Trace.** Paste a value; each lane ends up in one of four states:
  - **reached**: the value is there;
  - **awaiting**: the container carries the key but not this value, so the
    data hasn't arrived (or never will);
  - **not on this key's path**: the container doesn't have the key at all,
    so being empty means nothing;
  - **failed**: the query couldn't run; the reason is shown.
  Only the first documents of each lane are fetched; the view says when there
  are more. **Copy the document as JSON** copies one card. Recent values are
  remembered per account (**Forget these values** clears them).
- Several traces can be open side by side, each in its own window.

## Permissions

Reading the documents is a **data-plane** permission, separate from normal
Azure RBAC: someone with Reader or even Owner on the account can still get a
**403** when the app reads containers. The fix is the Cosmos DB **Built-in
Data Reader** SQL role assignment on the account. The app offers **Grant
myself Data Reader**, which works only if the user is allowed to create role
assignments on that account; otherwise whoever administers it has to grant
it.

## When something fails

1. **Get the exact message** from the screen.
2. **Check the session**: "Your Azure session has expired. Sign in again and
   rescan" means exactly that. `az account show` says who is signed in.
3. **Check data access**: a 403 is the data-plane role above.
4. **Look up the symptom** in `references/troubleshooting.md`. Many "the
   trace is wrong" reports are a key or lane choice, not a bug.
5. If it's still unexplained, check the version (window title or update
   banner): the release notes at
   <https://mayorana.ch/en/apps/ais-tracing/releases> say which version fixed
   what. Then offer to draft a report (below).

## Reporting a problem to the author

When the problem looks like an AIS Tracing bug, or the user wants to send
feedback, help them write a report they can send. Go through "When something
fails" first, even when the user asks straight for a report: a permission,
session or key-choice cause on their side wastes their time and the
author's, and finding it is more useful to them than a report. The user sends
it, not you: never create an issue, send an email or submit a form on their
behalf.

Traces show real documents: account, database and container names,
correlation ids, customer data in the documents themselves. Before showing
the draft, replace anything like that with neutral placeholders (`<account>`,
`<database>`, `<container>`, `<id>`), and describe document shapes by their
field names rather than pasting documents. Tell the user what you replaced
and ask them to check the rest.

Use this structure:

~~~markdown
**AIS Tracing version:** 0.1.27
**OS:** macOS 15.1 / Windows 11 / Ubuntu 24.04

**Setup**
Correlation key: <field> · Order by: <field> · Lanes: containers / <field>

**What I did**
1. …

**What I expected**
…

**What happened instead**
…

**Message shown**
```
…
```

**Workaround found, if any**
…
~~~

Then give them the two ways to send it:

- A GitHub issue at <https://github.com/bennekrouf/ais-tracing/issues/new>:
  they paste the title and body and submit it themselves. Issues there are
  public, which is one more reason the draft must be scrubbed.
- The contact form at <https://mayorana.ch/en/contact>, for anything they
  would rather not post publicly.
