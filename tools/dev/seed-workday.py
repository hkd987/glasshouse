"""Seeds a workday into a development glassd's home (dev_home), for agents
on a real model to work with: seven messages in the Inbox, this week's
calendar, two launch notes. The demo invoices come from host-run.sh.

usage: python3 -I tools/dev/seed-workday.py <home> <login>

The formats are the local Mail store and Calendar's event files, as
apps/e2e/global-setup.ts (seedInbox, seedCalendar) writes them. Dates are
relative to today; Date headers are written in UTC."""
import datetime as dt
import json
import os
import sys

home, login = sys.argv[1], sys.argv[2]
today = dt.date.today()
day = lambda n: today + dt.timedelta(days=n)
at = lambda d, hm: f"{d.isoformat()}T{hm}"

inbox = os.path.join(home, "Mail", "Inbox")
os.makedirs(inbox, exist_ok=True)
mail = [
    ("renewal", "Dana Ruiz <dana.ruiz@northwind.example>", "Renewal terms for next year", -1, "16:40",
     "Hi,\n\nBefore our call today, could we move the renewal start to January 1 instead of December 1? "
     "Legal on our side needs the extra month. If that works, please confirm by Friday and I'll send the "
     "updated order form.\n\nThanks,\nDana"),
    ("globex", "Globex Accounts <accounts@globex.example>", "Reminder: INV-2026-003 is overdue", -1, "09:12",
     "Hello,\n\nOur records show invoice INV-2026-003 for $310.25 was due on September 28 and is still unpaid. "
     "Please arrange payment or let us know if there is a problem.\n\nGlobex Facilities, Accounts"),
    ("one-on-one", "Priya Shah <priya@acme.example>", "Can we move our 1:1?", 0, "07:55",
     "Hey, something came up at 2 today. Could we move our 1:1 to tomorrow afternoon? Any time after 2 works "
     "for me.\n\nPriya"),
    ("deck", "Sam Okafor <sam@acme.example>", "Launch review deck", 0, "08:30",
     "I've got the launch review deck nearly done for tomorrow. The only open item is the launch date: "
     "Ana Lindqvist (ana.lindqvist@example.com) still has to confirm November 14 with the pilot org. "
     "Can you chase her today?\n\nSam"),
    ("maintenance", "IT Help Desk <it@acme.example>", "Scheduled maintenance this Saturday", -2, "11:00",
     "The VPN will be down for maintenance on Saturday from 08:00 to 10:00. No action needed.\n\nIT"),
    ("lunch", "Mei Chen <mei@acme.example>", "Lunch Friday?", 0, "09:05",
     "Still on for lunch tomorrow at 12:30? I booked the usual place.\n\nMei"),
    ("digest", "Vendor News <news@vendor.example>", "Your weekly digest", -1, "06:00",
     "Ten tips for a tidier inbox, and what's new this month.\n\nUnsubscribe anytime."),
]
for i, (slug, sender, subject, d, hm, body) in enumerate(mail):
    when = dt.datetime.combine(day(d), dt.time.fromisoformat(hm))
    path = os.path.join(inbox, f"{int(when.timestamp())}-{i}-{slug}.eml")
    with open(path, "w") as f:
        f.write(f"From: {sender}\nTo: {login}@acme.example\nSubject: {subject}\n"
                f"Date: {when.isoformat()}Z\n\n{body}\n")

events = os.path.join(home, "Calendar", "Events")
os.makedirs(events, exist_ok=True)
me = f"{login}@acme.example"
cal = [
    ("standup-today", "Team standup", at(day(0), "09:30"), at(day(0), "09:45"), "event", login, [me, "sam@acme.example", "mei@acme.example"], "Video call", ""),
    ("one-on-one-priya", "1:1 with Priya", at(day(0), "14:00"), at(day(0), "14:30"), "event", login, [me, "priya@acme.example"], "Room 2B", ""),
    ("northwind-renewal", "Northwind renewal call", at(day(0), "16:00"), at(day(0), "17:00"), "event", "dana.ruiz@northwind.example", ["dana.ruiz@northwind.example", me], "Video call", "Renewal terms and start date"),
    ("forecast-review", "Q4 forecast review", at(day(1), "10:00"), at(day(1), "11:00"), "event", login, [me, "pat@acme.example", "jordan@acme.example"], "Room 4A", ""),
    ("lunch-mei", "Lunch with Mei", at(day(1), "12:30"), at(day(1), "13:30"), "hold", login, [], "", ""),
    ("launch-review", "Launch review", at(day(1), "15:00"), at(day(1), "16:00"), "event", login, [me, "sam@acme.example", "ana.lindqvist@example.com"], "Room 4A", "Confirm the pilot launch date"),
    ("standup-monday", "Team standup", at(day(4), "09:30"), at(day(4), "09:45"), "event", login, [me, "sam@acme.example", "mei@acme.example"], "Video call", ""),
]
for eid, title, start, end, kind, organizer, attendees, location, notes in cal:
    with open(os.path.join(events, f"{eid}.json"), "w") as f:
        json.dump({"id": eid, "title": title, "start": start, "end": end, "kind": kind, "location": location,
                   "notes": notes, "attendees": attendees, "organizer": organizer, "attribution": ""}, f)

notes = os.path.join(home, "LaunchNotes")
os.makedirs(notes, exist_ok=True)
with open(os.path.join(notes, "plan.txt"), "w") as f:
    f.write("Q4 plan: ship the desktop agent to the pilot org by November 14.\n")
with open(os.path.join(notes, "todo.txt"), "w") as f:
    f.write("Call Ana (ana.lindqvist@example.com) about the launch date. Book the demo room for Thursday.\n")
print(f"seeded {len(mail)} messages, {len(cal)} events, 2 notes into {home} for {login}; today is {today}")
