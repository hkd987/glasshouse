# Glasshouse Agents on AWS

Glasshouse Agents runs in your own AWS account. Each agent works on a computer
of its own, under rules your IT team sets, and every step is on the record. One
CloudFormation stack (`glasshouse-agents.yaml`) sets it up. Nothing runs on our
servers, and your data doesn't leave your account.

This page is for the IT admin who deploys it. It covers what to prepare, how to
deploy, what it costs on your AWS bill, and how to update and delete it safely.

## What the stack puts in your account

- **A new VPC** across two Availability Zones:
  - Public subnets hold the load balancer and the egress relay.
  - The agent hosts are in private subnets with **no route to the internet**:
    no internet gateway route and no NAT. They reach AWS services through VPC
    endpoints, a few named addresses outside through the relay, and nothing
    else.
- **An egress relay**, the stack's only door out to the internet:
  - A small proxy (squid on Amazon Linux 2023) in a public subnet.
  - It lets the hosts reach four addresses only: three of Microsoft's, for
    Teams, and the load balancer's public signing keys. It refuses
    everything else.
  - It logs every request, allowed or refused, to the stack's log group.
  - Only the agent hosts can reach it, through a fixed internal address.
- **A load balancer** (Application Load Balancer) at your address:
  - HTTPS only; plain HTTP is redirected.
  - People sign in with your company's SSO (Entra ID, or IAM Identity Center)
    before reaching anything. The CLI's and Teams' requests carry tokens the
    service checks itself (see
    [How people and tools sign in](#how-people-and-tools-sign-in)).
- **Agent hosts**: EC2 instances in an Auto Scaling group.
  - They run the agents' computers as small virtual machines (microVMs).
  - They are managed through Systems Manager, so there is no SSH and no open
    port beyond the load balancer.
- **An S3 bucket** where agents' computers are kept while they sleep:
  - Encrypted with a KMS key the stack creates in your account.
  - Not public, and reachable only over TLS.
- **VPC endpoints** for S3, Amazon Bedrock, KMS, Secrets Manager, Systems
  Manager, STS, CloudWatch Logs and, on a Marketplace deployment, AWS
  Marketplace metering.
- **A CloudWatch Logs group** for the hosts' logs and the relay's log,
  encrypted with the same key.

Deleting the stack **keeps** the bucket, the key and the log group, so it never
deletes anyone's computers (see [Deleting safely](#deleting-safely)).

## Before you start

You need the following:

1. **A Region and capacity.**
   - The hosts must be **C8i, M8i or R8i** instances. Only these give a
     virtual instance the nested virtualization the computers need.
   - Check that your Region offers them in at least two Availability Zones.
   - Check that your EC2 quota for On-Demand standard instances (vCPUs)
     covers the most hosts you'll allow.
   - Amazon Bedrock must be available in the same Region.

2. **An address and a certificate.**
   - Choose an address in your domain, such as `agents.example.com`.
   - Request a public certificate for it in AWS Certificate Manager (ACM),
     **in the same Region**, and validate it.
   - Note the certificate's ARN.

3. **A sign-in app in your identity provider.** The load balancer signs people
   in with OpenID Connect (OIDC). You need the issuer, the authorization, token
   and user info endpoints, a client id, and a client secret.
   - **Microsoft Entra ID**
     1. App registrations → New registration, single tenant.
     2. Add a **Web** redirect URI: `https://<your address>/oauth2/idpresponse`.
     3. Certificates & secrets → New client secret.
     4. The endpoints, with your tenant id:
        - Issuer: `https://login.microsoftonline.com/<tenant id>/v2.0`
        - Authorization: `https://login.microsoftonline.com/<tenant id>/oauth2/v2.0/authorize`
        - Token: `https://login.microsoftonline.com/<tenant id>/oauth2/v2.0/token`
        - User info: `https://graph.microsoft.com/oidc/userinfo`
   - **IAM Identity Center**
     - Identity Center's own applications use SAML, which the load balancer
       doesn't take. Put an Amazon Cognito user pool in between:
       1. Create a user pool with a domain.
       2. Add Identity Center to it as a SAML identity provider. In Identity
          Center, add a custom SAML application whose ACS URL is
          `https://<cognito domain>/saml2/idpresponse` and whose audience is
          `urn:amazon:cognito:sp:<user pool id>`.
       3. Create an app client with a client secret, the authorization code
          grant, the scopes `openid email profile`, and the callback URL
          `https://<your address>/oauth2/idpresponse`.
     - The endpoints:
       - Issuer: `https://cognito-idp.<region>.amazonaws.com/<user pool id>`
       - Authorization: `https://<cognito domain>/oauth2/authorize`
       - Token: `https://<cognito domain>/oauth2/token`
       - User info: `https://<cognito domain>/oauth2/userInfo`
   - **Store the client secret in Secrets Manager**, as plain text, in the
     same account and Region:
     `aws secretsmanager create-secret --name glasshouse-agents/sign-in --secret-string '<the secret>'`.
     Note the secret's ARN. Only the load balancer uses it; the hosts can't
     read it.

4. **Bedrock models, enabled once.**
   - The hosts call Amazon Bedrock with their own role. On purpose, that role
     can't subscribe your account to a model.
   - So, before deploying, an admin enables each model you'll use. In the
     Bedrock console: Model catalog → choose the model → open it in the
     playground and send one message, in this Region.
   - Anthropic's models also ask for a one-time use-case form per account (or
     once for your AWS Organization).

5. **Microsoft Teams (optional).** To let agents ask people in Teams:
   1. Register a bot in your Entra tenant (an Azure Bot resource with a single
      tenant app registration).
   2. Note its Microsoft App ID.
   3. Store its client secret in Secrets Manager as plain text and note the
      ARN.
   4. After the stack is up, you'll set the bot's messaging endpoint. Your
      Teams admin installs the Glasshouse Agents app for everyone.
   - The app registration needs no Microsoft Graph permissions: the bot
     only sends to people who have the app.
   - The Teams app's manifest needs, besides its name, developer and icons
     (192 px colour, 32 px outline):
     - `bots`: `[{"botId": "<the App ID>", "scopes": ["personal", "groupChat"], "isNotificationOnly": false}]`
     - `permissions`: `["identity", "messageTeamMembers"]`
   - Your Teams admin uploads it to your org's app catalog, then installs it
     for everyone with an app setup policy. Each person's install gives
     Glasshouse Agents their chat with the app: their agents' questions
     arrive there, and only that person's answers count.
   - Leave both Teams parameters empty to deploy without Teams.

6. **The host image.**
   - On AWS Marketplace, the listing gives the AMI for your Region.
   - Otherwise, we send you the AMI id.

Secrets you create for this stack should use Secrets Manager's default key
(`aws/secretsmanager`). The stack's own key doesn't exist until the stack
does.

## Deploy

**In the console:**

1. CloudFormation → Create stack → With new resources.
2. Upload `glasshouse-agents.yaml`.
3. Fill in the parameters (below).
4. Acknowledge that the stack creates IAM resources (the hosts' and the
   relay's roles).
5. Create. It takes about 10 to 15 minutes.

**With the AWS CLI:**

The template is larger than the 51,200 bytes CloudFormation takes inline.
`aws cloudformation deploy` uploads it to an S3 bucket of yours first:

```sh
aws cloudformation deploy \
  --stack-name glasshouse-agents \
  --template-file glasshouse-agents.yaml \
  --s3-bucket <a bucket of yours for templates> \
  --capabilities CAPABILITY_IAM \
  --parameter-overrides \
    DomainName=agents.example.com \
    CertificateArn=arn:aws:acm:... \
    OidcIssuer=https://login.microsoftonline.com/<tenant id>/v2.0 \
    OidcAuthorizationEndpoint=https://login.microsoftonline.com/<tenant id>/oauth2/v2.0/authorize \
    OidcTokenEndpoint=https://login.microsoftonline.com/<tenant id>/oauth2/v2.0/token \
    OidcUserInfoEndpoint=https://graph.microsoft.com/oidc/userinfo \
    OidcClientId=<client id> \
    OidcClientSecretArn=arn:aws:secretsmanager:... \
    HostAmiId=ami-...
```

**When it's done** (the stack's Outputs tab has every value):

1. In your DNS, point your address at `LoadBalancerDnsName`: a CNAME, or a
   Route 53 alias record. The stack doesn't change your DNS.
2. Check that `SignInRedirectUrl` is a redirect URL in your sign-in app.
3. Open `Url` and sign in.
4. With Teams: set your bot's messaging endpoint to `TeamsMessagingEndpoint`.
5. To look at a host or the relay: Systems Manager → Session Manager → Start
   session. There is no SSH.
6. To see what the relay let out and what it refused: the log group
   `HostLogGroupName`, streams named `egress-relay/<instance id>`. A refused
   request reads `TCP_DENIED/403`.

## How people and tools sign in

- **In a browser:** people sign in through your SSO at the load balancer
  before anything they ask for reaches a host.
- **The `glass` CLI and the API:**
  - `glass login` opens the browser, where the person signs in through SSO
    as above. The service then hands the CLI a short-lived token of its own,
    for that person.
  - The CLI, and scripts or tools using the API, send that token as
    `Authorization: Bearer ...` to paths under `/api/v1/`. The load balancer
    passes those requests on without SSO, and the service checks the token
    itself.
  - A request to `/api/v1/` without that header, such as from a browser,
    still signs in through SSO.
- **Microsoft Teams:** Microsoft's Bot Framework posts to
  `/api/teams/messages` (POST only), without SSO. The service checks
  Microsoft's signed token itself.

## Parameters

| Parameter | What to give |
|---|---|
| `DomainName` | The address people open, such as `agents.example.com`. |
| `CertificateArn` | The ACM certificate for that address, in this Region. |
| `OidcIssuer` | Your identity provider's issuer URL. |
| `OidcAuthorizationEndpoint` | Its authorization endpoint. |
| `OidcTokenEndpoint` | Its token endpoint. |
| `OidcUserInfoEndpoint` | Its user info endpoint. |
| `OidcClientId` | The sign-in app's client (application) id. |
| `OidcClientSecretArn` | The Secrets Manager ARN of the sign-in app's client secret. |
| `HostAmiId` | The Glasshouse Agents host image for this Region. |
| `HostInstanceType` | A C8i, M8i or R8i size. The default is `m8i.xlarge` (4 vCPUs, 16 GiB). More memory per host means more computers awake at once; R8i has the most per vCPU. |
| `MinHosts` | Hosts always running. The default is 1. Keep it at 1 for now: an agent's computer is awake on one host at a time, and a request that reaches another host is refused until hosts pass requests on to each other. |
| `MaxHosts` | The most hosts the group grows to when busy. The default is 1, for the same reason. Must be at least `MinHosts`. |
| `HostDiskGiB` | Each host's disk, a cache of the computers that are awake. The default is 200. |
| `IdleMinutesBeforeSleep` | Minutes an agent's computer may sit idle before it is saved to S3 and stops costing anything but storage. The default is 15. |
| `TeamsAppId` | Your Teams bot's Microsoft App ID, or empty for no Teams. |
| `TeamsAppSecretArn` | The Secrets Manager ARN of the bot's client secret, or empty. Give both Teams values or neither. |
| `VpcCidr` | The new VPC's address range, /16 to /18. The default is `10.80.0.0/16`. Pick one that doesn't overlap networks you'll connect it to. |
| `RelayImage` | The egress relay's image, as an SSM parameter name. The default is AWS's latest Amazon Linux 2023. It is looked up when you create or update the stack, so the relay changes only with an update. To pin an image, name an SSM parameter of your own that holds its id. |
| `MarketplaceProductCode` | Filled in by the AWS Marketplace listing. Leave it empty outside Marketplace. |

## What it costs on your AWS bill

These are the kinds of charges, at your account's AWS prices. Use the AWS
Pricing Calculator for figures in your Region.

- **EC2 instances for the hosts.** On-Demand by the hour: `MinHosts` all the
  time, up to `MaxHosts` when busy. Savings Plans and Reserved Instances
  apply.
- **EBS storage.** One gp3 volume of `HostDiskGiB` per host.
- **The load balancer.** By the hour and by use (load balancer capacity
  units), plus AWS's charge for each public IPv4 address it holds.
- **Interface VPC endpoints.** Eight, or nine with Marketplace metering. Each
  is charged per Availability Zone per hour (two zones here), plus per GB
  through it. After the hosts, this is usually the largest fixed cost.
- **The egress relay.** Small, fixed and always on:
  - One `t3.micro` instance, its small gp3 volume, and its public IPv4
    address.
  - An internal Network Load Balancer that gives it a fixed address. It is
    charged by the hour and by use (capacity units), and usually costs more
    than the relay instance itself.
  - Traffic through it is small: tokens, signing keys and Teams messages,
    some of it crossing between Availability Zones.
- **S3.**
  - Storage for computers that are asleep, which is where an idle agent
    lives.
  - Requests when computers go to sleep and wake.
  - Traffic between the hosts and S3 stays in the Region through the gateway
    endpoint, with no data transfer charge.
- **KMS.** One key a month, plus requests. S3 bucket keys keep the request
  count low.
- **CloudWatch Logs.** The hosts' logs and the relay's log of requests,
  ingested and kept for 365 days.
- **Secrets Manager.** The secrets you created, a month each, plus calls.
- **Amazon Bedrock.** The tokens agents use, at your Bedrock prices. This is
  usually the largest cost that grows with use.
- **Data transfer out.** What the load balancer sends people: pages and live
  screens.
- **Glasshouse Agents itself.** Through AWS Marketplace, on the same bill: the
  contract, and agent computer hours above the amount it includes.

There is no NAT gateway, so there are no NAT charges.

## Updating

- **To change a parameter or move to a new template version:** CloudFormation
  → the stack → Update. Use a change set to see what will change first.
- **A new host image or instance type, or changed settings, replaces the
  hosts one at a time.**
  - Each new host starts before an old one goes. An old host waits 15 minutes
    before it goes, putting its awake computers to sleep in S3, so an update
    takes 20 minutes or more.
  - Someone using a computer while its host is replaced sees it pause and
    come back on a new host, with its programs where they were.
  - A computer is awake on one host at a time. Until the old host has put
    one to sleep, the new host won't wake it: its person is told it's awake
    on the other host, and to ask again in a moment.
  - Not even a one-host stack goes down for an update.
- **To change the sign-in client secret:**
  - CloudFormation reads the secret only when the load balancer's listener
    changes, so changing the secret's value in Secrets Manager isn't enough.
  - Store the new value in a new secret and update the stack with its ARN in
    `OidcClientSecretArn`.
- **The egress relay's image** is looked up again at every update. When AWS has
  released a newer Amazon Linux 2023 image since the last one, the update
  replaces the relay too. The new relay is in service before the old one
  goes, and its address doesn't change.
- **Don't change `VpcCidr`** on a running stack. It replaces the network and
  everything in it.

## Deleting safely

1. **Put every computer to sleep first.**
   - Scale the hosts to zero:
     `aws autoscaling update-auto-scaling-group --auto-scaling-group-name <the stack's group> --min-size 0 --max-size 0 --desired-capacity 0`.
   - Wait until the group has no instances. Each host saves its awake
     computers to S3 as it goes.
2. **Delete the stack.** It removes the network, the load balancer, the hosts,
   the egress relay, the roles and the endpoints.
3. **What stays:**
   - The bucket (`ComputersBucket`), the key (`DataKeyArn`) and the host log
     group (`HostLogGroupName`). These keep costing storage, and the key its
     monthly charge.
   - Your secrets, certificate, DNS record and sign-in app. The stack never
     touched them.
4. **To erase people's computers for good:**
   1. Empty and delete the bucket.
   2. Schedule the key's deletion. AWS waits 7 to 30 days, and you can cancel
      until then.
   - Once the key is gone, nothing it encrypted can ever be read again,
     anywhere. Check your retention obligations first.
5. **If you might come back:**
   - A new stack makes a new, empty bucket and key.
   - To bring the old computers back, the old bucket and key have to be
     imported into the new stack. Talk to us before deleting.

## What talks to what

For your security review:

- **People → the load balancer:** HTTPS (443). Port 80 only redirects.
- **The load balancer → your identity provider:** HTTPS to its token and user
  info endpoints, which are on the internet.
- **The load balancer → the hosts:** HTTPS (8443), inside the VPC. The hosts
  accept nothing else.
- **The hosts → AWS:** only through the VPC endpoints listed above.
  - The S3 endpoint's policy lets them write to this stack's bucket and
    nowhere else.
  - The hosts' role allows:
    - that bucket and that key;
    - `bedrock:InvokeModel` and `bedrock:InvokeModelWithResponseStream`;
    - writing their own logs;
    - reading the Teams secret, with Teams on;
    - `aws-marketplace:MeterUsage`, on Marketplace;
    - Systems Manager's managed-instance policy.
- **The hosts → the egress relay → the internet:** port 3128 to the relay's
  internal address, then HTTPS (443) to these exact names only:

  | Address | Why |
  |---|---|
  | `login.botframework.com` | Microsoft's keys for checking that a Teams request really came from Microsoft |
  | `login.microsoftonline.com` | Entra ID tokens for the Teams app, to send messages and cards |
  | `smba.trafficmanager.net` | Microsoft's Bot Connector service, where Teams messages are sent |
  | `public-keys.auth.elb.<region>.amazonaws.com` | The load balancer's public keys, for checking who signed in |

  - Anything else is refused and logged. That includes other hosts, other
    ports, plain HTTP, subdomains of these names and bare IP addresses.
  - The hosts use the relay only for these calls; nothing sets it as their
    system-wide proxy.
  - The relay also reaches AWS's Amazon Linux package repository, for its own
    packages, and Systems Manager and CloudWatch Logs through the VPC
    endpoints.
- **Agents' computers:** no network device at all. Everything they do outside
  goes through the host, under your policy, on the record.
- **People's tools → the load balancer:** requests under `/api/v1/` that carry
  `Authorization: Bearer ...` skip SSO. The service checks the token itself
  (see [How people and tools sign in](#how-people-and-tools-sign-in)).
- **Microsoft → the load balancer:** with Teams on, Microsoft's Bot Framework
  posts to `/api/teams/messages`, without SSO. The service checks Microsoft's
  signed token itself.

## Known gaps in this version

- **Microsoft's government clouds aren't on the relay's list.** Teams in GCC,
  GCC High and DoD uses other Microsoft hosts. This version supports Teams in
  Microsoft's commercial cloud only.
- **There is one relay.**
  - If it fails, its group starts another in a few minutes.
  - Until then, Teams messages wait, and so does a sign-in that needs a
    signing key the service hasn't fetched before.
- **One host.** With more than one, a request can reach a host other than
  the one a person's computer is awake on, and is refused there ("awake on
  another host") instead of passed on. Keep `MinHosts` and `MaxHosts` at 1
  until hosts pass requests on to each other.
- **A host that fails with computers awake** takes what they did since they
  last slept. Another host wakes each from its last sleep, two minutes
  after the failed host last answered.

## What only AWS can confirm

This template passes `cfn-lint` (`make aws-lint`), which checks every resource
and property against AWS's published schemas. These still need a real account,
and some a Marketplace listing:

- **Nested virtualization.** Hosts launched by the Auto Scaling group must
  have it on: `/dev/kvm` exists, and `aws ec2 describe-instances` shows
  `CpuOptions.NestedVirtualization` as `enabled`.
- **Metering through the VPC endpoint.**
  - Is MeterUsage through `com.amazonaws.<region>.metering-marketplace`
    accepted?
  - Does the endpoint's private DNS answer for
    `metering.marketplace.<region>.amazonaws.com`?
  - Marketplace's seller guide still says metered AMI software needs an
    internet gateway.
- **The S3 endpoint policy.** Does it leave Systems Manager and the host
  image's package updates working?
- **S3's conditional put.** A host takes a computer with a put that S3
  must refuse when the object is there (`If-None-Match: *`). It was run
  here against moto. On AWS: is it refused with 412 under the bucket's
  policy and KMS key, through the S3 endpoint?
- **S3's conditional delete.** The daily clear-out of chunks no sleeping
  computer names deletes each only if it is still the object it listed
  (`If-Match` with the entity tag). On AWS: does a chunk put again under
  the KMS key get a new tag, and is the delete then refused with 412?
- **The listing.** Does it take contract pricing with a metered
  `agent_computer_hours` dimension? Do the AMI and template pass AWS's scans?
- **The Bot Framework and TLS.** Does Microsoft's Bot Framework connect under
  the load balancer's TLS policy (`ELBSecurityPolicy-TLS13-1-2-Res-PQ-2025-09`)?
- **The egress relay on AWS.** Its squid configuration was run here with
  squid 6.14: the four names tunnel, everything else got 403 and was logged.
  On AWS, these still need checking:
  - The Amazon Linux 2023 image's own squid (5.8 when it was added) and its
    CloudWatch agent install and start.
  - Refused requests reach the log group through the CloudWatch Logs
    endpoint.
  - The relay sees the hosts' own addresses through its Network Load
    Balancer (client IP preservation), which its source rule expects.
  - The update policy starts a new relay before the old one goes.
- **The CLI path.** A bearer request to `/api/v1/` reaches a host without SSO,
  and the same request without the header is sent to sign in.

## For Glasshouse engineers: what the hosts are told

These are the stack's side of the contract with the host image.

**What the host image must provide:**

- Its root device is `/dev/xvda`.
- It has `openssl`.
- It runs `glassd.service` and `glass-agents.service` (systemd).
- The front serves HTTPS on port 8443 and answers `GET /healthz` with 200,
  without sign-in.

**The front's paths, as the load balancer routes them:**

- `POST /api/teams/messages` arrives without SSO (Teams on). Check the Bot
  Framework's token: OpenID metadata at
  `https://login.botframework.com/v1/.well-known/openidconfiguration`, keys
  refreshed at least daily, issuer `https://api.botframework.com`, audience
  the app id.
- `/api/v1/*` with `Authorization: Bearer ...` arrives without SSO.
  - Accept only tokens the front issued itself, and refuse anything else
    with 401.
  - On this path, take no identity from `x-amzn-oidc-*` headers: these
    requests never passed the load balancer's sign-in.
- Everything else has passed SSO. Verify `x-amzn-oidc-data` (ES256) and its
  `signer` before trusting a claim.
- Cache both sets of keys by key id, so replacing the relay doesn't
  interrupt sign-in.

**Files the user data writes on a host's first boot:**

- `/etc/glasshouse/glass-agents.toml`, for the front:
  - `egress_proxy = "http://<relay address>:3128"`, top level. An HTTPS
    CONNECT proxy, used only for the Microsoft and load balancer key calls
    above. It refuses any other host.
  - `[front]`: the URL, the listen address, TLS files and the health path.
  - `[sign_in]`: `kind = "alb-oidc"`, the load balancer's ARN (check the JWT
    header's `signer`), the issuer, the client id and the public-key URL.
  - `[teams]`: `enabled`, `app_id`, `secret_arn` and `messages_path`.
  - `[aws]`: the Region and the stack.
- `/etc/glasshouse/glassd.stack.toml`, read beside glassd's own config:
  - `[computers]`: Firecracker, an S3 store, the bucket, the KMS key ARN, the
    Region, `sleep_after_idle_minutes`.
  - `[models]`: Bedrock, in the Region.
  - `[metering]`: `enabled`, `product_code`, `dimension =
    "agent_computer_hours"`, the Region.
  - `[logs]`: the CloudWatch Logs group.
- A self-signed certificate in `/etc/glasshouse/tls/`. The load balancer
  doesn't check it; it only encrypts the hop.

**Credentials:**

- They come from the instance role through IMDSv2 (hop limit 1).
- `MeterUsage` must be signed with them; AWS refuses long-term keys for it.

**Termination:**

- A host leaving the group sees `Terminated` at instance metadata
  `autoscaling/target-lifecycle-state`. Poll it often.
- It has 15 minutes to put its computers to sleep.
- The hosts have no Auto Scaling endpoint or permission to finish the hook
  early (`CompleteLifecycleAction`), so a leaving host always waits the full
  15 minutes. Adding both would speed up updates.

**Metering:**

- `glass_gateway_providers::marketplace` sends one `MeterUsage` per host per
  hour.
- The service keeps one record per instance, dimension and hour, and takes
  hours up to six hours old.

## Sources

- EC2 nested virtualization:
  <https://docs.aws.amazon.com/AWSEC2/latest/UserGuide/amazon-ec2-nested-virtualization.html>
- The launch template's `CpuOptions.NestedVirtualization`:
  <https://docs.aws.amazon.com/AWSCloudFormation/latest/TemplateReference/aws-properties-ec2-launchtemplate-cpuoptions.html>
- Load balancer sign-in:
  <https://docs.aws.amazon.com/elasticloadbalancing/latest/application/listener-authenticate-users.html>
- Services with VPC endpoints:
  <https://docs.aws.amazon.com/vpc/latest/privatelink/aws-services-privatelink-support.html>
- `MeterUsage`:
  <https://docs.aws.amazon.com/marketplacemetering/latest/APIReference/API_MeterUsage.html>
- Metering for AMI products:
  <https://docs.aws.amazon.com/marketplace/latest/userguide/custom-metering-with-mp-metering-service.html>
- Bedrock model access:
  <https://docs.aws.amazon.com/bedrock/latest/userguide/model-access.html>
- Listener rule conditions (header and path matching):
  <https://docs.aws.amazon.com/elasticloadbalancing/latest/application/rule-condition-types.html>
- Network Load Balancer security groups:
  <https://docs.aws.amazon.com/elasticloadbalancing/latest/network/load-balancer-security-groups.html>
- Amazon Linux 2023 on EC2 (image parameters):
  <https://docs.aws.amazon.com/linux/al2023/ug/ec2.html>
- Bot Connector authentication (the Microsoft hosts):
  <https://learn.microsoft.com/en-us/azure/bot-service/rest-api/bot-framework-rest-connector-authentication>
- Bots in Microsoft's government clouds:
  <https://learn.microsoft.com/en-us/azure/bot-service/how-to-deploy-gov-cloud-high>
