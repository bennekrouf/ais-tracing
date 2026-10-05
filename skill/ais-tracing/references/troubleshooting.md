# AIS Tracing troubleshooting

Known failures, grouped by where they show up. Each entry: what the user sees,
why, and what to do. Versions in brackets are where a fix arrived.

## Contents

- [Starting and signing in](#starting-and-signing-in)
- [Scanning an account](#scanning-an-account)
- [Setup: the proposed key](#setup-the-proposed-key)
- [Tracing a value](#tracing-a-value)

## Starting and signing in

**"Azure CLI ('az') not found on PATH."**
Install the Azure CLI (the Windows installer does it for you, 0.1.17). An app
started from Finder or the Start menu picks up the login shell's PATH
(0.1.10); restart the app after installing.

**"Not signed in" / "Your Azure session has expired. Sign in again and
rescan".**
Use **Sign in again** (or `az login`), then rescan. From 0.1.14 an expired
session is reported as one, instead of showing as an account with no
containers.

**"No Cosmos DB accounts in any of your subscriptions."**
The signed-in user can't see any account. Check `az account list` covers
the right tenant and subscriptions; with PIM, activate the role first.

## Scanning an account

**A scan fails with 403.**
The user can see the account but not read its data: Cosmos DB data access
is a separate SQL role assignment from Azure RBAC. **Grant myself Data
Reader** assigns the Built-in Data Reader role if the user may create role
assignments on the account; otherwise an administrator has to. A new
assignment can take a few minutes to apply; rescan after that.

**"No databases/containers found in this account."**
The account is empty, or (before 0.1.14) the session had expired. Sign in
again and rescan to rule that out.

**A "Not sampled" panel lists some containers.**
Reading those containers failed during the scan, and the panel gives the
error for each. The setup knows nothing about them, so they can't take part in
the key or the trace. A 403 there is the data-plane role above; anything else,
read the error and rescan.

## Setup: the proposed key

**"Nothing in the sample linked containers on its own."**
No field carries the same values across containers in the sample. Either the
containers don't share an identifier, or the sample is too small or too old
to contain the same flows. Pick the key by hand, or rescan when there is
recent traffic.

**The proposed key is wrong** (a status, a type, a constant).
Values shorter than a few characters, and values that appear in too many
fields, are ignored as identifiers, but a schema can still mislead it. Change
the correlation key in Setup ⚙; it's a plain dropdown.

**"No correlation key chosen yet — pick one in Setup ⚙ first."**
Open Setup ⚙ and pick one.

## Tracing a value

**"No document anywhere carries this value, in full or in part. Check the
value, or the key."**
Check the value was pasted whole (no quotes, no trailing space) and that it
belongs to the chosen key: an order number won't be found if the key is a
correlation GUID.

**A lane says "Not on this key's path".**
That container doesn't have the key field at all, so it can't hold the value
whatever happened. This isn't a failure. If it should be on the path, its
documents use a different field for the same value; pick lanes or the key
differently.

**A lane stays empty ("awaiting").**
The container has the key, but no document with this value: the data hasn't
arrived there, or the flow never goes that way. This is usually the answer
the user is looking for.

**"More documents carry this value than are drawn here" / "Only the first
documents of each lane were fetched".**
The view caps what it fetches per lane; a single flow producing more than
that is unusual and worth a look on its own.

**Every card is red, or none is.**
Red comes from the error rules in Setup ⚙, which the user defines. Check the
field and the value each rule compares.

**Long lane or field names push the layout around.**
Fixed in 0.1.15 and 0.1.22; update.
