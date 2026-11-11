# 0066. Glasshouse Agents: the product on AWS

Status: **Accepted, being built**. Each agent's own computer is replaced
by one computer per person with their agents as personas on it (ADR 0068).
Agent hosts become Glasshouse OS machines shared by many people, and
agents run as the person, not root (ADR 0073).
Earlier status: (2026-10-27; first slice built 2026-10-28). The owner: "let's build it
out here and see how it all works together. Really think about this as an
entire product that we can sell through AWS and get spend that large
companies already have allocated to AWS. This is just how we get them
using agents fully." Builds on ADR 0065 (agent computers).

## The product
**Glasshouse Agents**: agents that work for a company's people, each on a
computer of its own, inside the company's own AWS account, under rules IT
sets, every step on the record and undoable.

- **Who buys:** a large company's IT or platform team, from AWS
  Marketplace, on the AWS bill they already have.
- **Who uses it:** every employee. Non-technical people in the web app and
  Teams; technical people in the CLI and the API; IT in the admin console.
- **Why they can say yes:** nothing leaves their account. Their models
  (Bedrock), their keys (KMS), their sign-in (SSO), their audit record.
  An agent is root on its own computer and asks before anything leaves it.

## Selling through AWS
- **Their committed spend.** A large company has usually committed to
  spend with AWS (an EDP or a private pricing agreement). Eligible
  Marketplace purchases draw that commitment down, so buying Glasshouse
  Agents spends money already set aside. Whether a listing qualifies, and
  how much of a commitment Marketplace may draw (one source says 25%), is
  the buyer's contract's: confirm per deal.
- **The listing:** software the buyer deploys in their account, an AMI
  with a CloudFormation template (ADR 0065 §9). Enterprise deals go as
  private offers.
- **Pricing (proposed):** an annual contract per person who uses agents
  (Marketplace contract pricing, through AWS License Manager), with agent
  computer hours above an included amount metered (`MeterUsage`, one
  dimension: `agent_computer_hours`). Bedrock, EC2 and S3 are on the
  buyer's own AWS bill: more of their AWS spend going to work.
- **To list:** an AWS Marketplace seller account, the AMI and template
  through AWS's scans, the product's dimensions, and later the ISV
  programs for co-selling with AWS's account teams.

## How it's built
One stack in the buyer's account (ADR 0065 §9). Four parts, each built
from what Glasshouse already has:

1. **Agent hosts** run glassd's runtime: the policy engine, the approvals
   desk, the audit record, undo, the model gateway (Bedrock), the actions
   (mail, calendar, files, the help desk). Unchanged at its core: an agent
   reaches the person's world only through its task socket, at the tiers
   policy sets.
2. **Agent computers:** a sandbox backend where an agent keeps a computer
   of its own between tasks (`glass_platform::sandbox::ComputerSandbox`).
   - Its disk is kept; each task runs in it with the task's socket, and
     the task's grants end with the task (PA-2): what the person gave it
     stays in its computer as copies, by design (ADR 0065 §2).
   - Hypervisors behind it: **Firecracker** on AWS (KVM), and Linux
     namespaces (`unshare`: no network, its own root) where there's no
     KVM, for development and these sessions. The namespace backend is a
     real boundary, weaker than a VM.
   - Asleep, a computer lives in S3 as chunks (ADR 0065 §11); here, in a
     folder standing in for S3.
3. **The front** (`glass-agents`): the web app, the API the CLI uses, the
   Teams relay's endpoint, metering. It signs people in (SSO: in AWS the
   load balancer's OIDC with their Entra or Identity Center; here, a
   development sign-in) and calls glassd's control API for them.
4. **Ways in** (ADR 0065 §12, §13): the web app (`apps/agents`), Teams,
   the `glass` CLI, and the API.

People and agents: an **agent** is one of IT's templates (a role, a
catalog agent, the reach it gets) made for a person, with its computer. A
**conversation** is that agent's tasks, in order. A question an agent asks
is an approval on the desk; answered once, from anywhere.

## What's built here, and what needs more
Built and run in these sessions (no AWS account, no KVM, no Teams
tenant). `make agents-dev` runs it all on one machine.

- **Agent computers** (`glass-computers`, glassd's `computers` sandbox
  backend). A task names its computer (`TaskRequest.computer`); one that
  names none gets a computer that goes with it.
  - On **namespaces**: a computer is a folder, its disk. Each run gets
    fresh user, mount, PID, network, IPC and UTS namespaces over it:
    - root inside, its owner outside (`setpriv` drops to the owner
      before `unshare` maps them to root);
    - the host's `/usr` read-only;
    - no network: the task socket is its one way out;
    - a small `/etc` with nothing secret.
  - The computer outlives its tasks; each task's socket and processes
    don't (PA-2).
  - At glassd's start, any process an earlier run left in a computer is
    killed before any task starts.
  - Each run starts below the person's priority, with its memory capped
    at the computer's. A mount point the agent made a link is refused,
    never followed.
  - The containment probe (`glass-probe`) passes 27 of its 30 checks in a
    namespace computer. The other three are namespaces doing what they
    should, not leaks:
    - `/home` lists `user`: the empty path the development agent programs
      are mounted at;
    - a UDP socket binds, with no network behind it;
    - "pid 1" is the computer's own first process.

    A Firecracker computer is checked on a host with KVM.
  - Its person **takes over** the keyboard (new tasks wait), types lines
    that run as root there, and hands it back. All of it is on the
    record.
  - Agents run commands on their computer themselves (the desktop
    agent's `computer_run`); their output is the computer's **screen**.
- **Sleep and wake** (ADR 0065 §11).
  - A computer unused for `sleep_after_idle_minutes`, or put to sleep by
    its person, is saved whole to the chunk store and removed from the
    host, all but its record.
  - A task or its person wakes it. The last three versions are kept.
  - The store: content-addressed 4 MiB chunks, SHA-256 checked on
    restore, all-zero chunks never stored. Restores can't escape their
    folder (cap-std). Backed by a folder here, or S3 (SigV4, SSE-KMS,
    instance-role credentials through IMDSv2).
  - **Another host wakes it.**
    - A computer is known in the store by a key made with it: its
      owner's key (their uid here; their sign-in subject once ADR 0067 is
      built) and 64 random bits. Two computers never share versions.
    - Each sleep keeps its record beside its versions, after the version
      is saved and before the host forgets it.
    - A host that never had it finds the owner's records the first time
      they list their agents, or when a task names a computer it lacks.
      It shows them asleep, and a task wakes them from the store. A task
      that names a computer the host lacks costs an S3 listing of its
      owner's computers and one read for each of their records.
  - **A host being replaced hands its computers on.** glassd reads the
    instance's Auto Scaling lifecycle state from its metadata every 10
    seconds. On `Terminated` (the stack's `SleepBeforeTermination` hook,
    15 minutes):
    - it takes no new task and wakes nothing ("this host is being
      replaced: ask again in a minute, and another host takes it");
    - it puts each computer to sleep as its work ends, for up to 12
      minutes;
    - whatever is still awake then is recorded as losing its work since
      it last slept.

    Once leaving, a host stays leaving until glassd restarts. On AWS the
    host is terminated anyway; a development host set back to `InService`
    needs a restart.
  - `specs/tla/AgentComputers.tla` checks the lifecycle through glassd
    dying at any step. It found two bugs, both fixed:
    - a wake after a crash failed forever on what the crash left;
    - processes a dead glassd left ran on while their computer was saved.
- **Firecracker** (`glass-computers/src/firecracker`): the client for
  Firecracker v1.17's API, the jailer's command line, boot from disk,
  pause, full snapshots and resume. A snapshot from another Firecracker
  boots from disk instead, so programs are lost, never files.
  - `glass-vmexec` and `glass-vmguest` run a task inside the guest over
    vsock.
  - All tested against stand-ins.
- **The front** (`glass-agents`) serves the web app (`apps/agents`) and
  the API (`glass-agents-api`). It turns glassd's computers and tasks
  into agents and conversations, and follows glassd's events for the
  page's long poll.
  - Here it acts for whoever runs it. Its token is the sign-in link's
    cookie, or `Authorization: Bearer` for the CLI.
- **The `glass` CLI**: agents, templates, new, ask (it streams the
  computer's screen and prints the answer), show, approvals,
  approve/deny, screen, take-over, type, hand-back, undo, sleep, wake,
  login.
- **Teams**:
  - only Microsoft's signed calls get in (RS256 against the Bot
    Connector's keys, endorsements, tenant, `serviceUrl`);
  - the person's approvals arrive in their chat with the app as cards;
  - answers count only from the person's Entra object id;
  - cards answered elsewhere, or expired, update;
  - a message naming an agent asks it, and its answer comes back in the
    chat.
- **The stack** (`deploy/aws`):
  - VPC endpoints, and no internet route for the hosts;
  - the load balancer with OIDC sign-in;
  - an egress relay (squid) allowing only Microsoft's and the load
    balancer's key hosts;
  - S3 with KMS, kept when the stack is deleted;
  - hosts with nested virtualization.

  `make aws-lint` passes in seven regions.
- **Metering**:
  - each minute, glassd counts the computers awake on its host;
  - each hour, it reports their hours to AWS Marketplace (`MeterUsage`,
    dimension `agent_computer_hours`), signed with the instance role;
  - the count survives restarts, and an hour that didn't send is tried
    again for the six hours AWS takes it;
  - every report is on the record.

- **AWS on this machine** (`deploy/local`, `make aws-local`): moto for S3
  and KMS, and stand-ins for the instance metadata service, Marketplace
  metering (it checks each signature) and the load balancer's sign-in
  (ES256 `x-amzn-oidc-data`, with its public key). It runs in Docker, or
  as processes without it.
  - `make aws-local-test` runs the real clients against it: the store
    through S3 under KMS, and metering signed with the role's keys. Then
    glassd sleeps an agent's computer with 300 MB on it to S3 and wakes
    it the same, byte for byte (14.8 s and 5.3 s here, against moto).
  - The same test then tells host A it's being replaced. A puts the
    computer to sleep by itself (11 s) and refuses the next task. Host
    B, which never had the computer, lists it asleep and wakes it with
    every file the same.
  - It found three bugs, all fixed:
    - the CLI missed a quick task's screen;
    - dev builds hashed at a few MB/s;
    - a computer's key in the store (`u<uid>-<id>`) wasn't unique across
      hosts or a remade computer, so one could trim another's versions.

Needs an AWS account (and a Marketplace seller account), a Teams tenant
and a host with KVM: run each against the real thing.

### Next
- **The host's config**: glassd reads the stack's settings file
  (`glassd.stack.toml`) beside the image's own, and Bedrock signs with the
  instance role, as S3 and metering already do. Today the Bedrock
  provider reads keys from the secrets file.
- **The guest image** for Firecracker: its kernel, its root with the exec
  service and the vsock bridges, and an AMI with both.
- **The front behind the load balancer**:
  - check its signed `x-amzn-oidc-data` header against the keys reached
    through the relay;
  - issue the tokens `glass login` takes;
  - one front for many people (below).
- **Confidential asks**: glassd says which asks carry labelled data, so
  their detail stays out of Teams.
- **The front forwards to the host that has the computer** (below): a
  computer is awake on one host at a time (the lease, built; see the
  amendment of 2026-11-10), and a request that lands on another host is
  refused in words until the fronts pass it on.
  - Until then, the stack defaults to one host (`MinHosts` and
    `MaxHosts` 1).
  - In an instance refresh the old and new hosts overlap for a few
    minutes. A computer still busy on the old host is refused on the new
    one until the old one has put it to sleep, where it used to wake a
    second time from its last sleep and lose one copy's work.
- **The store**:
  - parallel transfers when a computer wakes (saving has them, see the
    amendment below), and connections kept open between requests: each
    chunk is still a new TLS connection;
  - a lock against GC deleting a chunk a save reused but hasn't recorded
    yet;
  - manifests and computers' records signed by the host, so a bucket
    writer can't change one. Today both are trusted as the bucket is, and
    only hosts' roles may write it.
- **Firecracker**: a root-owned socket folder, against the check-then-
  connect race on the jail's sockets.
- **The template**: it's over CloudFormation's inline size, so it's
  deployed from S3 (Marketplace does).

## Which host serves a person (decided: the front forwards; to build before more than one host)
The load balancer spreads requests over the hosts, with no stickiness, so
a person's next request can land on a host other than the one their
computer is awake on. Teams' calls and the CLI's carry no browser cookie.
The lease makes a second host refuse, which is safe but not useful.
The options were:
1. **The front forwards to the host that holds the lease.** Any host takes
   any request; the lease names the host, and the fronts forward over the
   VPC. Works for the browser, the CLI and Teams alike, and needs no new
   AWS service.
2. **Load balancer stickiness, plus a home host per person.** Cheap for the
   browser. The CLI and Teams still need forwarding, so it's option 1
   anyway, plus a cookie.
3. **One host per person** (or per team). Simplest to reason about, but it
   costs a host per person, and Marketplace bills computer hours, not
   hosts.

**Decided: 1.** It builds on the lease, keeps one rule for every way in,
and lets the group scale on load. What it still needs, in order:

- **Each host's address in its lease**, and the fronts reachable from each
  other over the VPC only.
- **A request a front can pass on and the next front can trust.** The
  load balancer's signed `x-amzn-oidc-data` header says who the person is;
  a front that checks it (under "Next") can check it again when another
  front hands it over, so no host has to believe another about who is
  asking. This is why forwarding waits for that check.
- **The list from the store.** A host lists the computers it has and the
  ones asleep in the store. One awake on another host and never yet asleep
  is in the store only as its record and lease: the list has to show it,
  as awake elsewhere.
- **Two computers made at once by one id on two hosts** (a form sent
  twice): each gets a key of its own today. The id needs a claim in the
  store like the lease's.

## Identity on a host (to settle before many people share one)
Here, the front acts for the one person who runs it, as the apps' bridge
does in development. On a shared host, each person needs an identity
glassd trusts: either a session process per signed-in person, started as
their account (the desktop's own model: the greeter starts one), or the
front named as the one caller allowed to say who a request is for. The
first reuses what's tested; the second is lighter. Decided before slice 4
of ADR 0065.

## Amended 2026-11-10 (audit 7.1): a host being replaced saves its computers in time

A leaving host put its computers to sleep one after another, and each save
asked S3 about every chunk and then uploaded the new ones, one request at a
time. With about ten computers awake the saves overran the twelve minutes,
and the rest went with their work since they last slept.

- **A save doesn't ask about chunks its computer's last version has.** Most
  of a computer is as it last slept, and each of those chunks was a request
  that said "yes". The last version is still in the store while the save
  runs, so what it names is there. A chunk someone deleted from the bucket
  by hand is the one thing this no longer finds: the new version names it
  as the last one did, and waking says which chunk is missing.
- **New chunks go up four at a time** (`store::CHUNKS_AT_ONCE`), and the
  version is written only once every one of them is in. One that fails
  fails the save, which waits for those on their way and writes no version.
- **A chunk is hashed once.** It was hashed for its name, again for S3's
  header and a third time for the signature, the last two for every
  attempt. Its name is that hash, and S3 checks it against what arrives.
- **Four computers sleep at once** as a host leaves
  (`runtime::SLEEPS_AT_ONCE`), and none is begun once the time is up.

Measured with `make aws-local-test` (moto): the 300 MB computer's first
save took 4.1 s where it took 14.8 s, and its wake on another host 2.2 s
where it took 5.3 s. That test now holds the first save to a budget (a
second per 10 MB, thirty at least). A runtime test puts six computers on a
host that is told to leave, with a store in which a save ends only once
four are under way together: one at a time, it fails.

Not done: connections kept open between requests, and anything measured
against S3 itself or on a host with real computers on it. `AgentComputers.tla`
is one computer and one glassd, and nothing in it changes: a save is still
all in the store before its version is, or not there at all.

## Amended 2026-11-10 (audit 7.3): one host at a time per computer, and a wake goes by the store

Two things were wrong, and one fix needed the other.

**A wake restored the version the host remembered.** Each host keeps a
record of the computers it has seen (`computer.json`), with the version
each last slept as. A wake restored that version. A host that put a
computer to sleep on Monday, after another host had woken it and put it to
sleep again on Tuesday, woke Monday's: Tuesday's work was gone from what
the person saw, with nothing said. Once the store had trimmed Monday's
version (it keeps three), the wake failed instead. Agents added to the
computer in between were lost the same way.

**Nothing said which host had a computer awake.** A second host could wake
it from its last sleep while the first still had it; whichever slept later
replaced the other's work. With one host this happens only while a host
is being replaced, when old and new overlap for minutes.

What a computer is, asleep, is now what the store says, and one host at a
time has it awake:

- **A wake reads the store's record** (`computers/<key>/record.json`) and
  restores the version it names. A host's own record is what it lists the
  computer by until then. A computer's record is in the store from when
  it's made, not from its first sleep, so its id is taken on every host.
- **A lease per computer, in the store** (`store/lease.rs`): an object per
  term, `computers/<key>/lease/<term>.json`, saying which host, since
  when, and when it last renewed. Taking a computer is making the next
  term's object, with a put S3 refuses if the object is there
  (`If-None-Match: *`): of two hosts that ask at once, S3 lets one
  through. No host reads, decides and writes over another's object.
  - The host that makes or wakes a computer takes its lease, renews it
    every 30 seconds while it's awake or on its way (`keep_leases`), and
    marks it released once it's asleep in the store.
  - Another host leaves the computer alone until the lease is released, or
    hasn't been renewed for 2 minutes (`LEASE_FOR`): a host that died with
    it awake. The person is told which host has it.
  - A host's name is its EC2 instance id; off AWS, a name it makes once
    and keeps beside its computers.
- **Only the holder writes.** A host acts on its lease for 75 seconds
  after the store last took a renewal (`TRUST_FOR`), well short of the
  lease. Past that it takes a new term before it writes a version or the
  record, and it's given one only if the newest term is still its own. If
  another host has had the computer since, even one that has put it to
  sleep and released it, this host's copy is no longer the computer.
- **A copy that lost its computer is set aside, not saved and not
  deleted.** No task starts in it, the tasks in it are stopped and say
  why, and once they've ended its folder moves to `.aside/` under the
  computers' folder (root only, kept a week). The host then lists the
  computer as the store has it. Both steps are on the audit record. This
  happens to a host cut off from S3 for more than two minutes while
  another host could reach it; work done in that copy since its last sleep
  is what's set aside.
- **A change to a sleeping computer's record** (an agent added or removed)
  is made in the store's record, holding the lease for as long as it
  takes. It was written to the host's own record only, and lost when
  another host woke the computer.

What it costs: one small put and one check per awake computer every 30
seconds (well under a tenth of a cent per computer-hour in S3 requests),
and one list and one put per wake.

What it leans on: the hosts' clocks agree to within the 45 seconds between
`TRUST_FOR` and `LEASE_FOR` (they keep time with the same service), and
the write of a computer's record follows its check by less than that.

Not changed: a host that dies with a computer awake still loses what the
computer did since it last slept. The lease only decides when another host
may wake it from there.

Modelled as `specs/tla/ComputerLease.tla`: two hosts, wakes and sleeps
step by step, hosts cut off, glassd restarting, a host going away. It
checks that no save replaces work another host did, that a host sure of
its lease has it, and that what is awake is the version the record names.
Its Old switches are the code before (no lease; the host's own record)
and two ways the lease could have been wrong (a cut-off host given the
lease back because nobody held it; trust as long as the lease): each
breaks the first of those. `AgentComputers.tla` is still the computer on
one host.
