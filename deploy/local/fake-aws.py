#!/usr/bin/env python3
"""Stand-ins for the parts of AWS a Glasshouse Agents host uses that moto
doesn't play (deploy/local/README.md). Development and tests only.

Each listens on this machine's loopback address, from FAKE_AWS_PORT_BASE
(default 7610, where moto is):

  +1  the instance metadata service (IMDSv2): a session token, the
      instance role's name and its temporary keys, and its Auto Scaling
      lifecycle state. A path that starts with a name (/host-a/latest/...)
      is that host's: each host's state is its own
  +2  AWS Marketplace metering (MeterUsage): checks the SigV4 signature
      against the instance role's keys and keeps what was reported, as the
      service does (one record per product, dimension and hour). Also
      /_fake/ready, /_fake/config, /_fake/records (DELETE forgets them),
      /_fake/s3?prefix= (what the bucket holds, and under which key;
      DELETE removes it), POST /_fake/lifecycle?host=&state= (Terminated
      starts a host's replacement) and /_fake/assertion?email=&exp_in=
      &signer=&stranger=1 (claims for tests: expired, another signer's,
      signed by a key AWS never published)
  +3  the load balancer: people sign in (a stand-in for the identity
      provider), and requests go on to the front with the claims the
      load balancer adds (x-amzn-oidc-data, signed ES256). Teams' path and
      /api/v1/ with a bearer token pass without signing in, as the stack's
      rules say
  +4  AWS's public keys for those claims, by key id, as
      https://public-keys.auth.elb.<region>.amazonaws.com/<kid> serves them

On start it makes the stack's bucket and KMS key in moto.

Needs Python 3.10+, cryptography and boto3 (both in moto's image).
"""

import base64
import calendar
import hashlib
import hmac
import html
import http.client
import http.server
import json
import os
import secrets
import socketserver
import sys
import threading
import time
import urllib.parse
import uuid

from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import ec
from cryptography.hazmat.primitives.asymmetric.utils import decode_dss_signature

HOST = "127.0.0.1"
BASE = int(os.environ.get("FAKE_AWS_PORT_BASE", "7610"))
REGION = os.environ.get("FAKE_AWS_REGION", "us-east-1")
ACCOUNT = os.environ.get("FAKE_AWS_ACCOUNT", "123456789012")
BUCKET = os.environ.get("FAKE_AWS_BUCKET", "glasshouse-agents-local")
PRODUCT_CODE = os.environ.get("FAKE_AWS_PRODUCT_CODE", "glasshouse-local")
ROLE = "glasshouse-agents-host"
UPSTREAM = os.environ.get("FAKE_ALB_UPSTREAM", "http://127.0.0.1:7500")
SIGNER = (
    f"arn:aws:elasticloadbalancing:{REGION}:{ACCOUNT}:"
    "loadbalancer/app/glasshouse-agents-local/0123456789abcdef"
)
ISSUER = os.environ.get("FAKE_OIDC_ISSUER", "https://login.example.com/glasshouse-local/v2.0")
CLIENT_ID = "glasshouse-agents-local"
# Who can sign in: "email=Name,email=Name".
PEOPLE = [
    tuple(p.split("=", 1))
    for p in os.environ.get(
        "FAKE_PEOPLE", "dana@example.com=Dana Alvarez,sam@example.com=Sam Lee"
    ).split(",")
    if "=" in p
]
MOTO = f"http://{HOST}:{BASE}"
COOKIE = "AWSELBAuthSessionCookie-0"

# The instance role's keys, for this run.
KEYS = {
    "AccessKeyId": "ASIA" + "".join(secrets.choice("ABCDEFGHIJKLMNOPQRSTUVWXYZ234567") for _ in range(16)),
    "SecretAccessKey": secrets.token_urlsafe(30),
    "Token": base64.b64encode(secrets.token_bytes(48)).decode(),
}
IMDS_TOKENS = {}
# Each host's Auto Scaling lifecycle state, by the name its path starts with.
LIFECYCLE = {}
# The load balancer's signing key: AWS's, for this run.
SIGNING_KEY = ec.generate_private_key(ec.SECP256R1())
# A key AWS never published, for tests of forged claims.
STRANGER_KEY = ec.generate_private_key(ec.SECP256R1())
KID = str(uuid.uuid4())
STATE = {"ready": False, "kms_key_arn": None, "why": "starting"}
RECORDS = {}
SESSIONS = {}
LOCK = threading.Lock()


def log(*args):
    print("fake-aws:", *args, file=sys.stderr, flush=True)


def iso(at):
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(at))


def b64url(data):
    # The load balancer keeps base64's padding.
    return base64.urlsafe_b64encode(data).decode()


class Server(socketserver.ThreadingMixIn, http.server.HTTPServer):
    daemon_threads = True
    allow_reuse_address = True


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):
        log(self.server.name, fmt % args)

    def body(self):
        length = int(self.headers.get("Content-Length") or 0)
        return self.rfile.read(length) if length else b""

    def reply(self, status, body=b"", kind="text/plain; charset=utf-8", headers=()):
        if isinstance(body, str):
            body = body.encode()
        self.send_response(status)
        self.send_header("Content-Type", kind)
        self.send_header("Content-Length", str(len(body)))
        for name, value in headers:
            self.send_header(name, value)
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(body)

    def json(self, status, value, kind="application/json"):
        self.reply(status, json.dumps(value), kind)

    def do_GET(self):
        self.route()

    def do_HEAD(self):
        self.route()

    def do_POST(self):
        self.route()

    def do_PUT(self):
        self.route()

    def do_DELETE(self):
        self.route()

    def do_PATCH(self):
        self.route()


class Imds(Handler):
    """IMDSv2: a token first, and nothing without one."""

    def route(self):
        path = urllib.parse.urlsplit(self.path).path
        host = "default"
        if not path.startswith("/latest/"):
            host, _, rest = path.lstrip("/").partition("/")
            path = "/" + rest
        if self.command == "PUT" and path == "/latest/api/token":
            self.body()
            ttl = int(self.headers.get("X-aws-ec2-metadata-token-ttl-seconds") or 0)
            if not 1 <= ttl <= 21600:
                return self.reply(400, "")
            token = secrets.token_urlsafe(32)
            IMDS_TOKENS[token] = time.time() + ttl
            return self.reply(200, token, headers=[("X-aws-ec2-metadata-token-ttl-seconds", str(ttl))])
        token = self.headers.get("X-aws-ec2-metadata-token") or ""
        if IMDS_TOKENS.get(token, 0) < time.time():
            return self.reply(401, "")
        prefix = "/latest/meta-data/iam/security-credentials/"
        if path == prefix:
            return self.reply(200, ROLE)
        if path == prefix + ROLE:
            now = time.time()
            return self.json(200, {
                "Code": "Success",
                "LastUpdated": iso(now),
                "Type": "AWS-HMAC",
                **KEYS,
                "Expiration": iso(now + 6 * 3600),
            }, "text/plain")
        if path == "/latest/meta-data/placement/region":
            return self.reply(200, REGION)
        if path == "/latest/meta-data/instance-id":
            return self.reply(200, "i-0local" + hashlib.sha256(host.encode()).hexdigest()[:9])
        if path == "/latest/meta-data/autoscaling/target-lifecycle-state":
            return self.reply(200, LIFECYCLE.get(host, "InService"))
        self.reply(404, "")


def sigv4_problem(command, path, headers, body, service):
    """Why a request isn't signed with the instance role's keys, or None."""
    auth = headers.get("Authorization") or ""
    if not auth.startswith("AWS4-HMAC-SHA256 "):
        return "MissingAuthenticationTokenException", "Request is missing Authentication Token"
    parts = dict(p.strip().split("=", 1) for p in auth[len("AWS4-HMAC-SHA256 "):].split(","))
    key_id, date, region, svc, term = parts.get("Credential", "////").split("/")
    signed = parts.get("SignedHeaders", "").split(";")
    if key_id != KEYS["AccessKeyId"]:
        return "UnrecognizedClientException", "The security token included in the request is invalid."
    if (headers.get("X-Amz-Security-Token") or "") != KEYS["Token"] or "x-amz-security-token" not in signed:
        return "UnrecognizedClientException", "The security token included in the request is invalid."
    if region != REGION or svc != service or term != "aws4_request" or "host" not in signed:
        return "InvalidSignatureException", f"Credential should be scoped to {REGION}/{service}."
    amz_date = headers.get("X-Amz-Date") or ""
    try:
        at = calendar.timegm(time.strptime(amz_date, "%Y%m%dT%H%M%SZ"))
    except ValueError:
        return "IncompleteSignatureException", "X-Amz-Date is missing or not a date"
    if abs(time.time() - at) > 15 * 60 or not amz_date.startswith(date):
        return "InvalidSignatureException", "Signature expired"
    canonical_headers = "".join(
        f"{h}:{' '.join((headers.get(h) or '').strip().split())}\n" for h in signed
    )
    request = "\n".join([
        command, path, "", canonical_headers, ";".join(signed), hashlib.sha256(body).hexdigest(),
    ])
    scope = f"{date}/{region}/{service}/aws4_request"
    to_sign = "\n".join([
        "AWS4-HMAC-SHA256", amz_date, scope, hashlib.sha256(request.encode()).hexdigest(),
    ])
    key = ("AWS4" + KEYS["SecretAccessKey"]).encode()
    for part in (date, region, service, "aws4_request"):
        key = hmac.new(key, part.encode(), hashlib.sha256).digest()
    want = hmac.new(key, to_sign.encode(), hashlib.sha256).hexdigest()
    if not hmac.compare_digest(want, parts.get("Signature", "")):
        return "InvalidSignatureException", (
            "The request signature we calculated does not match the signature you provided."
        )
    return None


# Refusals AWS sends as 403.
DENIED = {
    "MissingAuthenticationTokenException", "UnrecognizedClientException",
    "InvalidSignatureException", "IncompleteSignatureException",
}


class Metering(Handler):
    """MeterUsage, as the service answers it; and the stand-ins' own state."""

    def route(self):
        path = urllib.parse.urlsplit(self.path).path
        if self.command == "GET" and path == "/_fake/ready":
            return self.json(200 if STATE["ready"] else 503, STATE)
        if self.command == "GET" and path == "/_fake/config":
            return self.json(200, config())
        query = urllib.parse.parse_qs(urllib.parse.urlsplit(self.path).query)
        if path == "/_fake/s3" and self.command in ("GET", "DELETE"):
            prefix = (query.get("prefix") or [""])[0]
            if self.command == "DELETE":
                if not prefix:
                    return self.reply(400, "a prefix, please")
                s3_remove(prefix)
            return self.json(200, s3_report(prefix))
        if self.command == "GET" and path == "/_fake/assertion":
            # Tests' own assertions: expired, another signer, a stranger's key.
            email = (query.get("email") or ["dana@example.com"])[0]
            person = {"sub": sub_for(email), "email": email, "name": (query.get("name") or [email])[0]}
            return self.reply(200, assertion(
                person,
                exp_in=int((query.get("exp_in") or ["120"])[0]),
                signer=(query.get("signer") or [SIGNER])[0],
                key=STRANGER_KEY if query.get("stranger") else SIGNING_KEY,
            ))
        if self.command == "POST" and path == "/_fake/lifecycle":
            self.body()
            host = (query.get("host") or ["default"])[0]
            LIFECYCLE[host] = (query.get("state") or ["InService"])[0]
            return self.json(200, LIFECYCLE)
        if path == "/_fake/records" and self.command in ("GET", "DELETE"):
            with LOCK:
                if self.command == "DELETE":
                    RECORDS.clear()
                return self.json(200, list(RECORDS.values()))
        body = self.body()
        if self.command != "POST" or path != "/":
            return self.reply(404, "")
        def error(kind, message):
            status = 403 if kind in DENIED else 400
            self.json(status, {"__type": kind, "message": message}, "application/x-amz-json-1.1")

        if self.headers.get("X-Amz-Target") != "AWSMPMeteringService.MeterUsage":
            return error("UnknownOperationException", "only MeterUsage is played here")
        problem = sigv4_problem("POST", "/", self.headers, body, "aws-marketplace")
        if problem:
            return error(*problem)
        try:
            usage = json.loads(body)
        except ValueError:
            return error("SerializationException", "not JSON")
        if usage.get("ProductCode") != PRODUCT_CODE:
            return error("InvalidProductCodeException", "The product code passed does not match")
        if usage.get("UsageDimension") != "agent_computer_hours":
            return error("InvalidUsageDimensionException", "The usage dimension does not match")
        stamp = int(usage.get("Timestamp") or 0)
        if not time.time() - 6 * 3600 <= stamp <= time.time():
            return error("TimestampOutOfBoundsException", "Timestamp is out of the allowed range")
        if usage.get("DryRun"):
            return error("DryRunOperation", "Request would have succeeded, but DryRun flag is set.")
        hour = stamp - stamp % 3600
        quantity = int(usage.get("UsageQuantity") or 0)
        with LOCK:
            found = RECORDS.get(hour)
            if found and found["quantity"] != quantity:
                return error(
                    "DuplicateRequestException",
                    "A metering record has already been created with different parameters",
                )
            if not found:
                found = RECORDS[hour] = {
                    "record_id": str(uuid.uuid4()),
                    "hour": hour,
                    "quantity": quantity,
                    "dimension": usage["UsageDimension"],
                    "signed_by": KEYS["AccessKeyId"],
                }
        self.json(200, {"MeteringRecordId": found["record_id"]}, "application/x-amz-json-1.1")


def sub_for(email):
    """A stable subject per person, as an identity provider gives."""
    return "sub-" + str(uuid.uuid5(uuid.NAMESPACE_URL, "mailto:" + email.lower()))


def assertion(person, exp_in=120, signer=None, key=None):
    """x-amzn-oidc-data: the load balancer's signed claims."""
    signer = signer or SIGNER
    key = key or SIGNING_KEY
    exp = int(time.time()) + exp_in
    header = {
        "typ": "JWT", "kid": KID, "alg": "ES256", "iss": ISSUER,
        "client": CLIENT_ID, "signer": signer, "exp": exp,
    }
    claims = {**person, "exp": exp, "iss": ISSUER}
    signed = (
        b64url(json.dumps(header, separators=(",", ":")).encode())
        + "."
        + b64url(json.dumps(claims, separators=(",", ":")).encode())
    )
    r, s = decode_dss_signature(key.sign(signed.encode(), ec.ECDSA(hashes.SHA256())))
    return signed + "." + b64url(r.to_bytes(32, "big") + s.to_bytes(32, "big"))


HOP = {"connection", "keep-alive", "proxy-authenticate", "proxy-authorization", "te",
       "trailers", "transfer-encoding", "upgrade", "content-length", "host"}


class LoadBalancer(Handler):
    """The stack's listener rules, with sign-in in front of everything else."""

    def route(self):
        url = urllib.parse.urlsplit(self.path)
        if url.path == "/oauth2/sign-in":
            return self.sign_in(url)
        if url.path == "/oauth2/sign-out":
            with LOCK:
                SESSIONS.pop(self.session_id(), None)
            return self.reply(303, "Signed out", headers=[
                ("Location", "/"), ("Set-Cookie", f"{COOKIE}=; Path=/; Max-Age=0")])
        # TeamsMessagesRule and ApiBearerRule skip sign-in.
        bearer = (self.headers.get("Authorization") or "").lower().startswith("bearer ")
        if (url.path == "/api/teams/messages" and self.command == "POST") or (
            url.path.startswith("/api/v1/") and bearer
        ):
            return self.forward(None)
        with LOCK:
            person = SESSIONS.get(self.session_id())
        if not person:
            self.body()
            return self.reply(302, "", headers=[
                ("Location", "/oauth2/sign-in?" + urllib.parse.urlencode({"next": self.path}))])
        self.forward(person)

    def session_id(self):
        for part in (self.headers.get("Cookie") or "").split(";"):
            name, _, value = part.strip().partition("=")
            if name == COOKIE:
                return value
        return None

    def sign_in(self, url):
        """The identity provider's page, standing in for Entra ID's."""
        if self.command == "POST":
            form = urllib.parse.parse_qs(self.body().decode())
            email = (form.get("email") or [""])[0].strip()
            name = (form.get("name") or [""])[0].strip() or email
            nxt = (form.get("next") or ["/"])[0]
            if not email or "@" not in email:
                return self.reply(400, "an email address, please")
            if not nxt.startswith("/") or nxt.startswith("//"):
                nxt = "/"
            sid = secrets.token_urlsafe(32)
            with LOCK:
                SESSIONS[sid] = {"sub": sub_for(email), "email": email, "name": name}
            return self.reply(302, "", headers=[
                ("Location", nxt), ("Set-Cookie", f"{COOKIE}={sid}; Path=/; HttpOnly; SameSite=Lax")])
        nxt = html.escape((urllib.parse.parse_qs(url.query).get("next") or ["/"])[0])
        buttons = "".join(
            f'<form method="post"><input type="hidden" name="next" value="{nxt}">'
            f'<input type="hidden" name="email" value="{html.escape(e)}">'
            f'<input type="hidden" name="name" value="{html.escape(n)}">'
            f'<button>{html.escape(n)} &lt;{html.escape(e)}&gt;</button></form>'
            for e, n in PEOPLE
        )
        self.reply(200, f"""<!doctype html><meta charset="utf-8">
<title>Sign in (local stand-in)</title>
<style>body{{font:16px system-ui;max-width:28rem;margin:4rem auto;padding:0 1rem}}
button{{display:block;width:100%;margin:.5rem 0;padding:.7rem;font:inherit}}</style>
<h1>Sign in</h1><p>A stand-in for your organization's sign-in (deploy/local).</p>
{buttons}
<form method="post"><input type="hidden" name="next" value="{nxt}">
<input name="email" placeholder="someone@example.com"> <input name="name" placeholder="Name">
<button>Sign in as someone else</button></form>""", "text/html; charset=utf-8")

    def forward(self, person):
        body = self.body()
        target = urllib.parse.urlsplit(UPSTREAM)
        upstream = http.client.HTTPConnection(target.hostname, target.port or 80, timeout=300)
        headers = {
            k: v for k, v in self.headers.items()
            if k.lower() not in HOP and not k.lower().startswith("x-amzn-oidc-")
        }
        headers["Host"] = self.headers.get("Host") or f"{HOST}:{BASE + 3}"
        headers["X-Forwarded-For"] = self.client_address[0]
        headers["X-Forwarded-Proto"] = "http"
        headers["X-Amzn-Trace-Id"] = f"Root=1-{int(time.time()):08x}-{secrets.token_hex(12)}"
        if person:
            headers["x-amzn-oidc-data"] = assertion(person)
            headers["x-amzn-oidc-identity"] = person["sub"]
            headers["x-amzn-oidc-accesstoken"] = secrets.token_urlsafe(32)
        try:
            upstream.request(self.command, self.path, body=body or None, headers=headers)
            got = upstream.getresponse()
            data = got.read()
        except OSError as e:
            return self.reply(502, f"the front didn't answer: {e}")
        finally:
            upstream.close()
        passed = [(k, v) for k, v in got.getheaders() if k.lower() not in HOP]
        self.send_response(got.status)
        for name, value in passed:
            self.send_header(name, value)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(data)


class PublicKeys(Handler):
    """AWS's keys for the load balancer's claims, by key id."""

    def route(self):
        if urllib.parse.urlsplit(self.path).path.strip("/") != KID:
            return self.reply(404, "")
        pem = SIGNING_KEY.public_key().public_bytes(
            serialization.Encoding.PEM, serialization.PublicFormat.SubjectPublicKeyInfo)
        self.reply(200, pem, "application/x-pem-file")


def config():
    return {
        "region": REGION,
        "s3_endpoint": MOTO,
        "bucket": BUCKET,
        "kms_key_arn": STATE["kms_key_arn"],
        "imds": f"http://{HOST}:{BASE + 1}",
        "metering": f"http://{HOST}:{BASE + 2}",
        "product_code": PRODUCT_CODE,
        "load_balancer": f"http://{HOST}:{BASE + 3}",
        "public_keys": f"http://{HOST}:{BASE + 4}",
        "signer": SIGNER,
        "people": [{"email": e, "name": n, "sub": sub_for(e)} for e, n in PEOPLE],
    }


def moto_client(service):
    import boto3

    return boto3.client(
        service, endpoint_url=MOTO, region_name=REGION,
        aws_access_key_id="moto-admin", aws_secret_access_key="moto-admin",
    )


def s3_report(prefix):
    """What the bucket holds under `prefix`, and how each object is encrypted."""
    s3 = moto_client("s3")
    report = {"objects": 0, "bytes": 0, "encryption": {}}
    for page in s3.get_paginator("list_objects_v2").paginate(Bucket=BUCKET, Prefix=prefix):
        for obj in page.get("Contents", []):
            head = s3.head_object(Bucket=BUCKET, Key=obj["Key"])
            how = head.get("ServerSideEncryption", "none")
            if how == "aws:kms":
                how += " " + head.get("SSEKMSKeyId", "?")
            report["objects"] += 1
            report["bytes"] += obj["Size"]
            report["encryption"][how] = report["encryption"].get(how, 0) + 1
    return report


def s3_remove(prefix):
    s3 = moto_client("s3")
    for page in s3.get_paginator("list_objects_v2").paginate(Bucket=BUCKET, Prefix=prefix):
        for obj in page.get("Contents", []):
            s3.delete_object(Bucket=BUCKET, Key=obj["Key"])


def make_bucket_and_key():
    """The stack's bucket and KMS key, in moto (idempotent)."""
    while True:
        try:
            kms = moto_client("kms")
            alias = "alias/glasshouse-agents-local"
            found = [a for a in kms.list_aliases()["Aliases"] if a["AliasName"] == alias]
            if found:
                arn = kms.describe_key(KeyId=found[0]["TargetKeyId"])["KeyMetadata"]["Arn"]
            else:
                arn = kms.create_key(Description="Glasshouse Agents data (local)")["KeyMetadata"]["Arn"]
                kms.create_alias(AliasName=alias, TargetKeyId=arn)
            s3 = moto_client("s3")
            if BUCKET not in [b["Name"] for b in s3.list_buckets()["Buckets"]]:
                s3.create_bucket(Bucket=BUCKET)
            s3.put_bucket_encryption(Bucket=BUCKET, ServerSideEncryptionConfiguration={"Rules": [{
                "ApplyServerSideEncryptionByDefault": {"SSEAlgorithm": "aws:kms", "KMSMasterKeyID": arn},
                "BucketKeyEnabled": True,
            }]})
            STATE.update(ready=True, kms_key_arn=arn, why="")
            log("ready: bucket", BUCKET, "key", arn)
            return
        except Exception as e:  # moto still starting
            STATE["why"] = f"waiting for moto at {MOTO}: {e}"
            time.sleep(0.5)


def main():
    servers = []
    for offset, handler in ((1, Imds), (2, Metering), (3, LoadBalancer), (4, PublicKeys)):
        server = Server((HOST, BASE + offset), handler)
        server.name = handler.__name__
        servers.append(server)
        threading.Thread(target=server.serve_forever, daemon=True).start()
    log(f"listening on {HOST}:{BASE + 1}-{BASE + 4}; the front at {UPSTREAM}")
    make_bucket_and_key()
    threading.Event().wait()


if __name__ == "__main__":
    main()
