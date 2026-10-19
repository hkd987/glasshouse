//! The Desktop artboard's data as control-API values: the three agents, the
//! sign-in user and a sample app window. Previews and snapshot tests draw
//! these; a live shell gets the same types from `task.list`.

use glass_api::{
    ActivityEntry, ActivityKind, AgentMode, ApprovalState, ApprovalView, AskLabels, ChangeEntry,
    ChangeKind, FeedLine, Progress, SnapshotMarker, TaskDetail, TaskView, TileState,
};
use glass_types::{ApprovalId, SnapshotId, TaskId, Timestamp};
use iced_core::alignment::{Horizontal, Vertical};
use iced_core::{Color, Length, Padding};
use iced_widget::{Column, column, container, row, stack, text};

use crate::screens::command_bar::{Answer, CommandBar};
use crate::screens::desktop::{Desktop, Tiles};
use crate::screens::expanded;
use crate::screens::login::SignIn;
use crate::theme::hex;
use crate::{Element, Message, Theme, fonts, icons, style};

/// The artboard's Monday afternoon, 1:54 PM UTC.
pub fn now() -> Timestamp {
    Timestamp::from_unix_seconds(1_792_418_040).unwrap_or_else(Timestamp::now)
}

fn minutes_ago(minutes: i64) -> Timestamp {
    Timestamp::from_unix_seconds(now().unix_seconds() - minutes * 60).unwrap_or_else(Timestamp::now)
}

fn line(verb: &str, target: &str) -> FeedLine {
    FeedLine {
        at: now(),
        text: format!("{verb} {target}"),
        verb: verb.into(),
        target: target.into(),
    }
}

/// The Desktop artboard's tiles: an expense agent that needs you, a
/// meeting-prep actions agent and an invoice screen agent.
pub fn tasks() -> Vec<TaskView> {
    let expense = TaskId::new();
    let base =
        |agent: &str, name: &str, mode, state, title: &str, caption: &str, started| TaskView {
            id: TaskId::new(),
            agent: agent
                .parse()
                .unwrap_or_else(|_| "agent".parse().expect("valid id")),
            agent_name: name.into(),
            user: "dana".into(),
            mode,
            state,
            title: title.into(),
            caption: caption.into(),
            progress: None,
            feed: Vec::new(),
            needs: None,
            receipt: None,
            failure: None,
            started_at: started,
            updated_at: now(),
            driver_socket: None,
            continues: None,
        };
    let mut needs = base(
        "expense-agent",
        "Expense agent",
        AgentMode::Actions,
        TileState::NeedsYou,
        "Expense report",
        "6 receipts matched in the expense portal. Nothing has been submitted.",
        minutes_ago(9),
    );
    needs.id = expense;
    needs.needs = Some(ApprovalView {
        id: ApprovalId::new(),
        task: expense,
        agent: "expense-agent".parse().expect("valid id"),
        action: "expenses.submit".parse().expect("valid action"),
        summary: "Submit your expense report for $412.60?".into(),
        items: [
            "Rideshare, airport to hotel · $38.40",
            "Hotel, 1 night · $214.00",
            "Client lunch, 3 people · $86.20",
            "Airport parking · $24.00",
            "Coffee before meeting · $12.40",
            "Train, hotel to client office · $37.60",
        ]
        .map(String::from)
        .to_vec(),
        rule: "org rule expenses.submit".into(),
        requested_at: minutes_ago(1),
        expires_at: now(),
        state: ApprovalState::Pending,
        labels: AskLabels::new("Review report", "Approve and submit"),
    });
    let mut prep = base(
        "meeting-prep",
        "Meeting prep",
        AgentMode::Actions,
        TileState::Working,
        "Northwind 2:00 PM brief",
        "Prepping your 2:00 PM with Northwind",
        minutes_ago(4),
    );
    prep.feed = vec![
        line("read", "Calendar · 2:00 PM Northwind review"),
        line("call", "crm.get_account Northwind"),
        line("write", "Briefs/Northwind 2pm.md"),
    ];
    let mut invoices = base(
        "invoice-agent",
        "Invoice agent",
        AgentMode::Screen,
        TileState::Working,
        "March invoices",
        "Reconciling March invoices",
        minutes_ago(2),
    );
    invoices.progress = Some(Progress {
        done: 12,
        total: 40,
    });
    vec![needs, prep, invoices]
}

/// Monday, October 19, `hour:minute` UTC.
fn at(hour: i64, minute: i64) -> Timestamp {
    const MIDNIGHT: i64 = 1_792_368_000;
    Timestamp::from_unix_seconds(MIDNIGHT + hour * 3600 + minute * 60)
        .unwrap_or_else(Timestamp::now)
}

/// The Expanded artboard's moment: 2:14 PM UTC, twelve minutes into the
/// invoice run.
pub fn expanded_now() -> Timestamp {
    at(14, 14)
}

/// The Expanded artboard: the invoice agent 12 invoices into 40, with its
/// activity, three snapshots, eleven changes (one email that can't be
/// undone) and what it can't touch. The artboard's agent drives the AP
/// app's screen; screen agents can't stream one yet, so this one works
/// through the app's actions and the view shows its feed.
pub fn invoice_detail() -> TaskDetail {
    let feed = |minute, verb: &str, target: &str| FeedLine {
        at: at(14, minute),
        text: format!("{verb} {target}"),
        verb: verb.into(),
        target: target.into(),
    };
    let view = TaskView {
        id: TaskId::new(),
        agent: "invoice-agent".parse().expect("valid id"),
        agent_name: "Invoice agent".into(),
        user: "dana".into(),
        mode: AgentMode::Actions,
        state: TileState::Working,
        title: "March invoices".into(),
        caption: "Reconciling March invoices in AP Invoice Entry".into(),
        progress: Some(Progress {
            done: 12,
            total: 40,
        }),
        feed: vec![
            feed(12, "match", "INV-0411 → PO 7779"),
            feed(14, "open", "INV-0412 · AP Invoice Entry"),
            feed(14, "open", "PO 7781 · Purchase orders"),
        ],
        needs: None,
        receipt: None,
        failure: None,
        started_at: at(14, 2),
        updated_at: expanded_now(),
        driver_socket: None,
        continues: None,
    };
    let entry = |minute, kind, text: &str| ActivityEntry {
        at: at(14, minute),
        kind,
        text: text.into(),
    };
    use ActivityKind::{Action, Approval, Notice, Snapshot};
    let activity = vec![
        entry(2, Action, "Opened AP Invoice Entry, batch MAR-26"),
        entry(4, Action, "Matched INV-0401 → INV-0405 to POs"),
        entry(5, Snapshot, "Snapshot saved"),
        entry(8, Notice, "Flagged INV-0407 — total is $212.40 over PO"),
        entry(8, Snapshot, "Snapshot saved"),
        entry(9, Approval, "Emailed Northwind AP about INV-0407"),
        entry(10, Action, "Matched INV-0406, INV-0408 → INV-0410"),
        entry(11, Snapshot, "Snapshot saved"),
        entry(12, Action, "Matched INV-0411 → PO 7779"),
        entry(14, Action, "Opened INV-0412 and PO 7781"),
    ];
    let matched = [
        (3, "0401", "7740"),
        (3, "0402", "7741"),
        (4, "0403", "7744"),
        (4, "0404", "7745"),
        (4, "0405", "7750"),
        (10, "0406", "7752"),
        (10, "0408", "7760"),
        (10, "0409", "7763"),
        (10, "0410", "7766"),
        (12, "0411", "7779"),
    ];
    let mut changes: Vec<ChangeEntry> = matched
        .into_iter()
        .map(|(minute, invoice, po)| ChangeEntry {
            id: 0,
            at: at(14, minute),
            kind: ChangeKind::Modified,
            target: format!("INV-{invoice} matched to PO {po}"),
            action: "ap.invoice.match".parse().expect("valid action"),
            reversible: true,
            paths: vec![format!("~/Invoices/MAR-26/INV-{invoice}.json")],
            undone: false,
        })
        .collect();
    changes.insert(
        5,
        ChangeEntry {
            id: 0,
            at: at(14, 9),
            kind: ChangeKind::Sent,
            target: "Email to Northwind AP about INV-0407".into(),
            action: "mail.send".parse().expect("valid action"),
            reversible: false,
            paths: Vec::new(),
            undone: false,
        },
    );
    for (id, change) in (1..).zip(changes.iter_mut()) {
        change.id = id;
    }
    let marker = |minute, reason: &str| SnapshotMarker {
        id: SnapshotId::new(),
        at: at(14, minute),
        reason: reason.into(),
    };
    TaskDetail {
        view,
        prompt: "Reconcile the March invoices in AP Invoice Entry against their purchase \
                 orders. Flag anything more than $50 off and let the vendor know."
            .into(),
        plan: [
            "Open batch MAR-26 in AP Invoice Entry",
            "Match each invoice to its purchase order",
            "Flag invoices whose totals are more than $50 off",
            "Email the vendor about flagged invoices, after you approve",
            "Save the matches as drafts for you to post",
        ]
        .map(String::from)
        .to_vec(),
        now: "Matching INV-0412 to PO 7781 — vendor, quantities and total agree".into(),
        activity,
        changes,
        cant_touch: ["Posting invoices", "Payments", "Vendor bank details"]
            .map(String::from)
            .to_vec(),
        snapshots: vec![
            marker(5, "After invoice 5"),
            marker(8, "Before emailing Northwind AP"),
            marker(11, "After invoice 10"),
        ],
    }
}

/// The expense agent's tile from [`tasks`] opened on its ask: the review
/// list and the ask's own answers.
pub fn expense_detail(task: &TaskView) -> TaskDetail {
    let minutes_before = |m: i64| {
        Timestamp::from_unix_seconds(task.updated_at.unix_seconds() - m * 60)
            .unwrap_or_else(Timestamp::now)
    };
    let entry = |m, kind, text: &str| ActivityEntry {
        at: minutes_before(m),
        kind,
        text: text.into(),
    };
    TaskDetail {
        view: task.clone(),
        prompt: "File my expenses for the Chicago client visit, Oct 13 to 14.".into(),
        plan: [
            "Find the trip's receipts in mail and photos",
            "Fill in a draft report in the expense portal",
            "Ask you before submitting it",
        ]
        .map(String::from)
        .to_vec(),
        now: task.needs.as_ref().map_or_else(String::new, |ask| {
            format!("Waiting for you to approve: {}", ask.summary)
        }),
        activity: vec![
            entry(
                9,
                ActivityKind::Action,
                "Found 6 receipts in mail and photos",
            ),
            entry(6, ActivityKind::Snapshot, "Snapshot saved"),
            entry(4, ActivityKind::Action, "Filled in draft report ER-2291"),
            entry(1, ActivityKind::Approval, "Asked you to submit $412.60"),
        ],
        changes: vec![ChangeEntry {
            id: 1,
            at: minutes_before(4),
            kind: ChangeKind::Created,
            target: "Draft report ER-2291".into(),
            action: "expenses.draft".parse().expect("valid action"),
            reversible: true,
            paths: Vec::new(),
            undone: false,
        }],
        cant_touch: ["Corporate card settings", "Other people's reports"]
            .map(String::from)
            .to_vec(),
        snapshots: vec![SnapshotMarker {
            id: SnapshotId::new(),
            at: minutes_before(6),
            reason: "Before the draft report".into(),
        }],
    }
}

/// The desktop under the expanded view: the tiles tucked away and the
/// agent as the focused app.
pub fn expanded_desktop<'a>(tasks: &'a [TaskView], agent: &str) -> Desktop<'a> {
    Desktop {
        app: agent.into(),
        menus: expanded::MENUS.map(String::from).to_vec(),
        tiles: Tiles::Hidden,
        ..desktop(tasks)
    }
}

pub fn desktop(tasks: &[TaskView]) -> Desktop<'_> {
    Desktop {
        tasks,
        now: now(),
        clock: "Mon Oct 19  2:14 PM".into(),
        app: "Browser".into(),
        menus: vec![
            "File".into(),
            "Edit".into(),
            "View".into(),
            "Window".into(),
            "Help".into(),
        ],
        date: (String::from("OCT"), 19),
        // The artboard's three tiles, as banners.
        tiles: Tiles::Banners(tasks.iter().collect()),
        wallpaper: true,
        system_menu: None,
        lockable: false,
        trash_full: false,
        selected: None,
    }
}

/// The Command bar artboard's recent tasks.
pub fn recent_tasks() -> Vec<TaskView> {
    let mut tasks = tasks();
    let mut it = tasks[0].clone();
    it.id = TaskId::new();
    it.agent_name = "IT agent".into();
    it.title = "VPN connection".into();
    it.state = TileState::Done;
    it.needs = None;
    it.updated_at = Timestamp::from_unix_seconds(now().unix_seconds() - 5 * 3600 + 120)
        .unwrap_or_else(Timestamp::now);
    let mut onboarding = tasks[1].clone();
    onboarding.id = TaskId::new();
    onboarding.agent_name = "Onboarding agent".into();
    onboarding.title = "Onboarding Alex Kim".into();
    onboarding.progress = Some(Progress { done: 3, total: 6 });
    tasks.truncate(1);
    tasks.extend([it, onboarding]);
    tasks
}

pub fn command_bar(recent: &[TaskView]) -> CommandBar<'_> {
    let plain = |s: &str| (s.to_owned(), false);
    CommandBar {
        query: "prep me for my 2:00".into(),
        answer: Some(Answer {
            paragraphs: vec![
                vec![
                    plain("Your 2:00 PM is the "),
                    ("Northwind quarterly review".into(), true),
                    plain(
                        " with Sam Ortiz and Priya Shah. Two open items: the $212.40 overage on INV-0407, \
                         which Northwind confirmed this morning, and their contract renewal due November 30.",
                    ),
                ],
                vec![plain(
                    "I can build a one-page brief with account history, the latest invoices and talking points.",
                )],
            ],
            accept: "Build the brief".into(),
            decline: "That's enough, thanks".into(),
            sources: "Sources: Calendar, CRM, Mail, Teams · read-only so far".into(),
        }),
        suggestions: vec![
            "Explain the Midwest variance".into(),
            "Turn this into a six-slide deck".into(),
            "Flag regions below plan".into(),
        ],
        recent,
        model: "Claude via Amazon Bedrock · Internal data".into(),
    }
}

pub fn sign_in() -> SignIn {
    SignIn {
        date: "Monday, October 19".into(),
        time: "8:02".into(),
        user: "Dana Ruiz".into(),
        initials: "DR".into(),
        managed_by: "[Org name] managed device · signs in with Entra ID".into(),
        battery_percent: Some(82),
        password: String::new(),
        message: None,
        fingerprint: true,
        switch_user: true,
        wallpaper: true,
    }
}

/// The screen agent's display on the artboard: the AP invoice entry form,
/// with the agent's labeled cursor in the amount field.
pub fn invoice_entry_thumbnail<'a>(theme: &Theme) -> Element<'a, Message> {
    let bar = container(
        row![
            text("AP · Invoice Entry").size(9).color(hex(0xE6EAEE)),
            iced_widget::space().width(Length::Fill),
            text("Batch MAR-26").size(9).color(hex(0xE6EAEE)),
        ]
        .align_y(Vertical::Center),
    )
    .width(Length::Fill)
    .padding(Padding::from([0.0, 8.0]))
    .center_y(Length::Fixed(20.0))
    .style(|_| style::panel(hex(0x2B3440), 0.0));
    let bar_line = |w: f32, color: u32, h: f32| {
        container("")
            .width(Length::Fixed(w))
            .height(Length::Fixed(h))
            .style(move |_| style::panel(hex(color), 2.0))
    };
    let sidebar = container(
        column![
            bar_line(40.0, 0x2B3440, 5.0),
            bar_line(46.0, 0xB9C2CC, 5.0),
            bar_line(34.0, 0xB9C2CC, 5.0),
            bar_line(42.0, 0xB9C2CC, 5.0)
        ]
        .spacing(6),
    )
    .width(Length::Fixed(64.0))
    .height(Length::Fill)
    .padding(Padding::from([8.0, 6.0]))
    .style(|_| style::panel(hex(0xE2E7EC), 0.0));
    let accent = theme.accent;
    let field = move |label: &'static str, value: &'static str, focused: bool| {
        column![
            text(label).size(8).color(hex(0x5F6875)),
            container(text(value).size(8).color(hex(0x14171C)))
                .width(Length::Fill)
                .center_y(Length::Fixed(14.0))
                .padding(Padding::from([0.0, 4.0]))
                .style(move |_| {
                    style::outlined(
                        hex(0xFFFFFF),
                        3.0,
                        1.0,
                        if focused { accent } else { hex(0xC9D0D8) },
                    )
                }),
        ]
        .spacing(2)
        .width(Length::Fill)
    };
    let form = column![
        text("Invoice INV-0412")
            .size(10)
            .font(fonts::SEMIBOLD)
            .color(hex(0x14171C)),
        row![
            field("Vendor", "Northwind Supply", false),
            field("PO number", "PO 7781", false)
        ]
        .spacing(10),
        row![
            field("Invoice date", "03/14/2026", false),
            field("Amount", "$18,420.00", true)
        ]
        .spacing(10),
        row![
            bar_line(44.0, 0xD9DFE5, 14.0),
            bar_line(32.0, 0x2B3440, 14.0)
        ]
        .spacing(6),
    ]
    .spacing(7)
    .padding(Padding::from([10.0, 12.0]));
    let cursor = container(
        row![
            icons::view(&icons::AGENT_CURSOR, accent, 14.0, 16.0),
            container(
                container(
                    text("agent")
                        .size(9)
                        .font(fonts::SEMIBOLD)
                        .color(Color::WHITE)
                )
                .padding(Padding::from([1.0, 5.0]))
                .style(move |_| style::panel(accent, 4.0))
            )
            .padding(Padding {
                top: 10.0,
                ..Padding::ZERO
            }),
        ]
        .spacing(2),
    )
    .padding(Padding {
        left: 236.0,
        top: 104.0,
        ..Padding::ZERO
    });
    container(stack![
        column![bar, row![sidebar, form].height(Length::Fill)],
        cursor
    ])
    .width(Length::Fill)
    .height(Length::Fixed(crate::tiles::THUMBNAIL_HEIGHT))
    .clip(true)
    .style(|_| style::panel(hex(0xEEF1F4), 8.0))
    .into()
}

/// The artboard's app window content: the Q3 forecast sheet. Apps are web
/// apps (UI-5); this stands in for one inside the shell's window frame.
pub fn forecast_sheet<'a>() -> Element<'a, Message> {
    let ink = hex(0x14171C);
    let muted = hex(0x5F6875);
    let green = hex(0x2E7D4F);
    let rust = hex(0xB4441C);
    let rows: [(&str, &str, &str, &str, &str); 6] = [
        ("Northeast", "$4.82M", "$5.01M", "$5.36M", "+3.1%"),
        ("Southeast", "$3.94M", "$4.10M", "$4.27M", "+1.8%"),
        ("Midwest", "$3.12M", "$3.05M", "$3.18M", "−0.6%"),
        ("Mountain", "$1.76M", "$1.84M", "$1.97M", "+4.4%"),
        ("West", "$5.40M", "$5.62M", "$5.88M", "+2.2%"),
        ("International", "$2.31M", "$2.18M", "$2.25M", "−2.9%"),
    ];
    let divider = |color: u32| {
        container("")
            .width(Length::Fill)
            .height(Length::Fixed(1.0))
            .style(move |_| style::panel(hex(color), 0.0))
    };
    let cell = |s: &'a str, color: Color, mono: bool| {
        container(
            text(s)
                .size(13)
                .color(color)
                .font(if mono { fonts::MONO } else { fonts::SANS }),
        )
        .width(Length::Fill)
        .align_x(if mono {
            Horizontal::Right
        } else {
            Horizontal::Left
        })
    };
    let head = row![
        container(text("Region").size(12).font(fonts::MEDIUM).color(muted)).width(Length::Fill),
        container(text("Q1 actual").size(12).font(fonts::MEDIUM).color(muted))
            .width(Length::Fill)
            .align_x(Horizontal::Right),
        container(text("Q2 actual").size(12).font(fonts::MEDIUM).color(muted))
            .width(Length::Fill)
            .align_x(Horizontal::Right),
        container(
            text("Q3 forecast")
                .size(12)
                .font(fonts::MEDIUM)
                .color(muted)
        )
        .width(Length::Fill)
        .align_x(Horizontal::Right),
        container(text("vs plan").size(12).font(fonts::MEDIUM).color(muted))
            .width(Length::Fill)
            .align_x(Horizontal::Right),
    ]
    .padding(Padding {
        bottom: 10.0,
        ..Padding::ZERO
    });
    let mut table: Column<'a, Message, iced_widget::Theme, iced_widget::Renderer> =
        column![head, divider(0xE1E5EA)];
    for (region, q1, q2, q3, plan) in rows {
        let plan_color = if plan.starts_with('+') { green } else { rust };
        table = table
            .push(
                row![
                    cell(region, ink, false),
                    cell(q1, ink, true),
                    cell(q2, ink, true),
                    cell(q3, ink, true),
                    cell(plan, plan_color, true),
                ]
                .padding(Padding::from([12.0, 0.0])),
            )
            .push(divider(0xEEF1F4));
    }
    table = table.push(
        row![
            cell("Total", ink, false),
            cell("$21.35M", ink, true),
            cell("$21.80M", ink, true),
            cell("$22.91M", ink, true),
            cell("+1.7%", green, true),
        ]
        .padding(Padding::from([14.0, 0.0])),
    );
    let tab = |label: &'a str, on: bool| {
        container(
            text(label)
                .size(13)
                .font(if on { fonts::MEDIUM } else { fonts::SANS })
                .color(if on { ink } else { hex(0x4A5361) }),
        )
        .padding(Padding::from([8.0, 12.0]))
        .style(move |_| {
            style::panel(
                if on {
                    hex(0xEEF1F4)
                } else {
                    Color::TRANSPARENT
                },
                6.0,
            )
        })
    };
    let toolbar = container(
        row![
            row![
                tab("Summary", true),
                tab("By region", false),
                tab("Vendors", false)
            ]
            .spacing(4),
            iced_widget::space().width(Length::Fill),
            text("Shared with Finance team · saved 2:13 PM")
                .size(12)
                .color(muted),
        ]
        .align_y(Vertical::Center),
    )
    .padding(Padding::from([0.0, 24.0]))
    .center_y(Length::Fixed(52.0))
    .width(Length::Fill);
    let commentary = container(
        column![
            text("Commentary").size(12).font(fonts::MEDIUM).color(muted),
            text("Midwest is soft on freight costs; holding the forecast flat until the March vendor invoices are reconciled")
                .size(14)
                .color(ink),
        ]
        .spacing(8),
    )
    .width(Length::Fill)
    .padding(Padding::from([16.0, 18.0]))
    .style(|_| style::outlined(Color::TRANSPARENT, 10.0, 1.0, hex(0xE1E5EA)));
    column![
        toolbar,
        divider(0xE8EBEF),
        container(table).padding(Padding {
            top: 24.0,
            left: 24.0,
            right: 24.0,
            bottom: 0.0
        }),
        container(commentary).padding(Padding {
            top: 20.0,
            left: 24.0,
            right: 24.0,
            bottom: 0.0
        }),
    ]
    .into()
}
