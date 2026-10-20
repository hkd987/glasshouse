// Mail's logic (UI-5, AU-10): names, dates, replies and the draft that
// answers a message.

import assert from "node:assert/strict";
import { test } from "node:test";

import {
  type Message,
  type Summary,
  address,
  displayName,
  draftFor,
  initials,
  listDate,
  moves,
  parseRecipients,
  problems,
  replyTo,
  viewUrl,
  wasSent,
} from "../src/mail.ts";

test("nv3 a message's HTML is framed on its own origin, and only a plain id gets one", () => {
  const here = { protocol: "http:", port: "7300" };
  assert.equal(
    viewUrl(here, "inbox/0896246782-0000000042.eml"),
    "http://view.mail.localhost:7300/inbox/0896246782-0000000042.eml",
  );
  for (const id of ["inbox/../x.eml", "inbox/.x.eml", "inbox/x.txt", "x.eml", "inbox/a/b.eml", "Inbox/x.eml"]) {
    assert.equal(viewUrl(here, id), null, id);
  }
});

test("em0 mail is filed in inbox, archive, junk or trash, and drafts stay put", () => {
  assert.deepEqual(
    moves("inbox").map((m) => m.to),
    ["archive", "junk", "trash"],
  );
  assert.deepEqual(moves("junk")[0], { to: "inbox", label: "Not junk" });
  assert.deepEqual(moves("drafts"), []);
  for (const folder of ["inbox", "sent", "archive", "junk", "trash"] as const) {
    assert.ok(
      moves(folder).every((m) => m.to !== folder),
      folder,
    );
  }
});

const dana: Message = {
  id: "inbox/1792400000-0-renewal-terms.eml",
  folder: "inbox",
  from: "Dana Ruiz <dana@northwind.example>",
  to: ["demo@corp.example"],
  subject: "Renewal terms",
  date: "2026-10-19T08:12:00Z",
  snippet: "Hi, can we talk terms?",
  attribution: null,
  body: "Hi,\n\nCan we talk terms?\n",
};

const draft: Summary = {
  id: "drafts/1792400100-0-re-renewal-terms.eml",
  folder: "drafts",
  from: "demo",
  to: ["dana@northwind.example"],
  subject: "Re: Renewal terms",
  date: "2026-10-19T08:20:00Z",
  snippet: "Tuesday works.",
  attribution: { text: "by claude-code for demo", by: "claude-code", for: "demo" },
};

test("names and addresses", () => {
  assert.equal(displayName(dana.from), "Dana Ruiz");
  assert.equal(displayName("dana@northwind.example"), "dana@northwind.example");
  assert.equal(displayName('"Ruiz, Dana" <dana@northwind.example>'), "Ruiz, Dana");
  assert.equal(address(dana.from), "dana@northwind.example");
  assert.equal(initials(dana.from), "DR");
  assert.equal(initials("ap@corp.example"), "AP");
});

test("a list's dates: today's time, this week's day, else the date", () => {
  const now = new Date(2026, 9, 19, 15, 0);
  assert.match(listDate(new Date(2026, 9, 19, 9, 5).toISOString(), now), /^9:05\sAM$/);
  assert.equal(listDate(new Date(2026, 9, 16, 9, 5).toISOString(), now), "Fri");
  assert.equal(listDate(new Date(2026, 9, 2, 9, 5).toISOString(), now), "Oct 2");
  assert.equal(listDate("not a date", now), "");
});

test("a reply goes to the sender, says Re: once, and quotes the message", () => {
  const reply = replyTo(dana);
  assert.deepEqual(reply.to, ["dana@northwind.example"]);
  assert.equal(reply.subject, "Re: Renewal terms");
  assert.match(reply.body, /Dana Ruiz wrote:\n> Hi,\n>\n> Can we talk terms\?\n$/);
  assert.equal(replyTo({ ...dana, subject: "RE: Renewal terms" }).subject, "RE: Renewal terms");
});

test("au10 the draft an agent wrote answers its message, until it is sent", () => {
  assert.equal(draftFor(dana, [draft]), draft);
  assert.equal(draftFor({ ...dana, from: "someone@else.example" }, [draft]), null);
  assert.equal(draftFor({ ...dana, subject: "Invoice" }, [draft]), null);
  const sent: Summary = { ...draft, id: "sent/1.eml", folder: "sent", date: "2026-10-19T09:00:00Z" };
  assert.equal(wasSent(draft, []), false);
  assert.equal(wasSent(draft, [sent]), true);
  assert.equal(wasSent(draft, [{ ...sent, date: "2026-10-18T09:00:00Z" }]), false, "an older one isn't it");
});

test("recipients and what a message lacks", () => {
  assert.deepEqual(parseRecipients("dana@northwind.example, ap@corp.example; x"), {
    to: ["dana@northwind.example", "ap@corp.example"],
    bad: ["x"],
  });
  assert.deepEqual(problems({ to: [], subject: " ", body: "" }, []), ["Add someone to send it to.", "Add a subject."]);
  assert.deepEqual(problems({ to: ["a@b.example"], subject: "Hi", body: "" }, ["x"]), ["Not an address: x"]);
});
