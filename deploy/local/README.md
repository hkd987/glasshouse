# AWS on this machine

What a Glasshouse Agents host uses from AWS (ADR 0065, 0066), simulated
on a developer's machine, so the agent-computer, storage, metering and
sign-in paths can be tested without an AWS account. Development and tests
only: nothing here ships.

```sh
make aws-local         # start it; writes build/local-aws/env
make aws-local-test    # agent computers to S3 and back, metering, against it
make aws-local-down    # stop it (moto forgets everything)
GLASS_DEV_STORE=s3 make agents-dev   # the dev stack, computers sleeping to it
```

It runs in Docker when a daemon answers, and otherwise as the same
programs in processes (`GLASS_LOCAL_AWS_PROCESSES=1` asks for that), from
a virtualenv in `build/tools/local-aws`. Everything listens on
`127.0.0.1` only. Both containers use the host's network, so the load
balancer can reach the front on loopback. Docker Desktop needs *Settings >
Resources > Network > Enable host networking*.

| Port | Stands in for | What the host does with it |
|---|---|---|
| 7610 | S3, KMS, Secrets Manager ([moto](https://github.com/getmoto/moto) 5.1.20, Apache-2.0) | Sleeping computers' chunks and versions, each object under the stack's KMS key (`store = "s3"` with `endpoint`) |
| 7611 | The instance metadata service (IMDSv2) | The instance role's temporary keys, which sign every S3 and metering call, and its Auto Scaling lifecycle state (`AWS_EC2_METADATA_SERVICE_ENDPOINT`). A path that starts with a name (`/host-a/latest/...`) is that host's own, so each host has its own lifecycle state. |
| 7612 | AWS Marketplace metering (`MeterUsage`) | Agent computer hours, hourly (`[metering] endpoint`). It checks each SigV4 signature against the role's keys and keeps records as the service does |
| 7613 | The load balancer with OIDC sign-in | A sign-in page stands in for Entra ID. Requests go on to the front (`GLASS_LOCAL_AWS_FRONT`, default `127.0.0.1:7500`) with `x-amzn-oidc-data` signed ES256, as ADR 0067 verifies. Teams' path and `/api/v1/` with a bearer token skip sign-in, as the stack's rules do |
| 7614 | `public-keys.auth.elb.<region>.amazonaws.com` | The load balancer's public key, by key id |

`fake-aws.py` is the stand-ins (Python, with `cryptography` and `boto3`
from moto's image). On start it makes the stack's bucket and KMS key in
moto. `GLASS_LOCAL_AWS_PORT` moves every port.

`build/local-aws/env` holds the endpoints, bucket, KMS key ARN, product code
and the load balancer's ARN as `GLASS_LOCAL_*`. `tools/dev/host-run.sh`
reads it for `GLASS_DEV_STORE=s3`. It never uses AWS keys from the
environment then: glassd gets only the stand-in metadata service, as an
agent host gets only its instance role. The stand-in's
`/_fake/s3?prefix=` reports what the bucket holds and under which key
(`DELETE` removes it), and `/_fake/records` what was metered. `POST
/_fake/lifecycle?host=host-a&state=Terminated` tells a host it's being
replaced, as the Auto Scaling group's lifecycle hook does.

## What it proves, and what it doesn't

- `make aws-local-test` runs the real clients against it:
  - the chunk store through S3 with SSE-KMS (incremental saves, removing
    versions, GC);
  - metering signed with the role's keys (dry run, duplicates, forged
    keys refused);
  - then `tests/local-aws/agents-s3.sh`: glassd sleeps an agent's computer
    with a few hundred MB on it to S3 and wakes it the same, byte for byte.
    Its host is then told it's being replaced, and puts the computer to
    sleep by itself. A second host, which never had it, wakes it.
  - then `tests/local-aws/people.sh`: two people behind the stand-in load
    balancer, each reaching only their own agents and computer, and every
    forged, expired or foreign proof refused (ADR 0067). It makes the
    front's account, `glass-agents`, as the stack does, so it runs as
    root.
- moto keeps objects in memory and doesn't enforce bucket policies or KMS
  key policies. Its speed says nothing about S3's, so time sleeps and wakes
  on AWS.
- The network isn't simulated: hosts here reach everything, where the
  stack gives them no route out but its VPC endpoints and the egress relay.
- Not simulated yet: the Bot Framework (the front's tests carry their own
  stand-in), Bedrock, and Firecracker (Linux namespaces stand in for it,
  as in `make agents-dev`).
