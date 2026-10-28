# 0070. IT turns apps off, for agents too

Status: **Accepted, built** (2026-10-28). The owner: "We will need to make
sure the IT admin can disable some of the default apps if they want,
example being maybe they want mail or calendar disabled by default."

## Context
- Which apps a person gets is already policy, by group (AD-7, role-based
  delivery): a group lists the apps its members get, and a person gets
  every app any of their groups lists. glassd opens no other app for them,
  and the launcher offers no other.
- That is a list of grants, so IT couldn't take an app away:
  - Unchecking Mail for Finance did nothing while Everyone listed Mail.
  - There was no "off for the org unless a group needs it".
- An app out of someone's profile still reached them through their
  agents. Agents get an app's actions through the broker at their tiers,
  and nothing tied those to the profile. Mail off in the launcher, a
  person's agent could still send mail for them.

## Decision
1. **Off, for the org:** `[org] apps_off = ["mail"]`. The app is in no
   one's profile unless a group they're in turns it back on.
2. **On or off, for a group:**
   - `apps_on = ["mail"]` turns an app the org turned off back on for
     the group's members.
   - `apps_off = ["calendar"]` turns it off for them whatever another
     group or the org says. **Off wins.**
   - A group can't turn one app both on and off: the policy is refused.
3. **Off means off everywhere for that person.**
   - The launcher doesn't offer it, and it doesn't open. A refusal is
     audited (`AppRefused`), as before.
   - The Store doesn't bring it back: neither an org app under its id
     nor the org's edition of ours. A person's remix of it stays shut,
     as it already did when its app left their profile.
   - **Every agent and every app acting for them finds its actions
     Never** (`RuleSource::AppOff`, checked first in `tier_for`, beside
     the people-only and person's-own rules):
     - its tools aren't listed;
     - a call is refused before the app sees its input, as a tool result
       the agent can read: "mail.send is blocked for this task (IT turned
       the mail app off for you)";
     - each refusal is on the record.
   - Turning it off takes effect when the release is applied:
     - an open Mail window closes;
     - a running task's next call to it is refused (PolicyApply,
       AD-3).
4. **The console:** the policy page's apps card.
   - On Everyone's page each app is On or Off: the org's.
   - On a group's page each app is "As the org has it (on/off)", On or
     Off.
   - The preview says what changes ("Everyone: apps turned off adds
     calendar"). The card lists Chat, Docs, Artifacts, Meet and Routines
     too.
5. An app's actions are its id's: `mail.*` are Mail's, `calendar.*`
   Calendar's. Actions of no app (files, people, models) aren't turned
   off this way; tiers do that, as before.

## Proof
- Policy (`adr0070_it_turns_apps_off_for_the_org_and_groups_and_off_wins`):
  - Mail off for the org, back on for Finance, off for contractors.
  - Someone in both Finance and contractors has it off.
  - Its actions are Never for an agent and for a person's own app, with
    why.
  - On and off in one group is refused.
- Runtime:
  - `adr0070_an_app_it_turned_off_is_off_for_their_agents_too`: Mail
    isn't offered and doesn't open. The agent's tool list has no
    `mail_*` tools, and its `mail_send` is refused with why and on the
    record, nothing sent. A group turns it back on; another turning it
    off wins.
  - `adr0070_the_store_doesnt_bring_back_an_app_it_turned_off`: neither
    an org app the catalog requires for everyone, nor the org's edition
    of Mail.
- Console:
  - `adr0070_mail_off_for_the_org_on_for_finance_off_for_contractors`:
    the edits.
  - The browser test `adr0070 IT turns Calendar off for the org, and the
    device stops offering it`: published to the pilot device within the
    propagation budget, a group's page showing the org's, and back on.
- On an agent host (`tests/local-aws/negative.sh`):
  - Mail is off for the org and on for Finance.
  - Sam's agent is offered no Mail tools, and nothing of his reaches
    Mail.
  - Dana's agent (Finance) asks her before sending; she says no and
    nothing goes out.
