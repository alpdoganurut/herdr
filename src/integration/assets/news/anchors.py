#!/usr/bin/env python3
"""herdr AI news: keyless source fetch ("anchors").

Reads sources.json, fetches every source concurrently with python stdlib only,
keeps items published inside the window, and writes anchors.json. Uses gzip and
ETag / Last-Modified (cached in http-cache.json) so unchanged feeds cost a 304.

usage: anchors.py --sources sources.json --out anchors.json [--hours 48] [--cache http-cache.json]
"""
import argparse, gzip, html, json, os, re, sys, time, urllib.error, urllib.request
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime, timedelta, timezone
from email.utils import parsedate_to_datetime
import xml.etree.ElementTree as ET

UA = ("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 "
      "(KHTML, like Gecko) Chrome/128.0 Safari/537.36")
ATOM = "{http://www.w3.org/2005/Atom}"
SITEMAP = "{http://www.sitemaps.org/schemas/sitemap/0.9}"
DC = "{http://purl.org/dc/elements/1.1/}"


def parse_date(s):
    if not s:
        return None
    try:
        d = parsedate_to_datetime(s)
    except (TypeError, ValueError):
        try:
            d = datetime.fromisoformat(s.strip().replace("Z", "+00:00"))
        except ValueError:
            return None
    return d if d.tzinfo else d.replace(tzinfo=timezone.utc)


def clean(s):
    s = re.sub(r"<[^>]+>", " ", s or "")
    return re.sub(r"\s+", " ", html.unescape(s)).strip()


def fetch(url, cache):
    """GET with gzip and conditional headers. Returns (status, body or None). https only: the
    source list can be rewritten by the editor agent, so file:// and internal hosts are refused."""
    if not url.startswith("https://"):
        raise ValueError("refusing a non-https source URL")
    headers = {"User-Agent": UA, "Accept-Encoding": "gzip", "Accept": "*/*"}
    c = cache.get(url, {})
    if c.get("etag"):
        headers["If-None-Match"] = c["etag"]
    if c.get("last_modified"):
        headers["If-Modified-Since"] = c["last_modified"]
    req = urllib.request.Request(url, headers=headers)
    try:
        with urllib.request.urlopen(req, timeout=20) as r:
            body = r.read()
            if r.headers.get("Content-Encoding") == "gzip":
                body = gzip.decompress(body)
            cache[url] = {"etag": r.headers.get("ETag"), "last_modified": r.headers.get("Last-Modified")}
            return r.status, body
    except urllib.error.HTTPError as e:
        if e.code == 304:
            return 304, None
        raise


def parse(src, body, now):
    t = src["type"]
    out = []  # (title, url, date, note)
    if t == "hn_algolia":
        for h in json.loads(body).get("hits", []):
            url = h.get("url") or "https://news.ycombinator.com/item?id=%s" % h["objectID"]
            note = "%d points, %d comments on HN" % (h.get("points") or 0, h.get("num_comments") or 0)
            out.append((h.get("title"), url, parse_date(h.get("created_at")), note))
    elif t == "substack_api":
        for p in json.loads(body):
            out.append((p.get("title"), p.get("canonical_url"), parse_date(p.get("post_date")), p.get("subtitle") or ""))
    elif t == "json" and "daily_papers" in src["url"]:
        for p in json.loads(body):
            pp = p.get("paper", {})
            out.append((pp.get("title") or p.get("title"), "https://huggingface.co/papers/%s" % pp.get("id", ""),
                        parse_date(p.get("publishedAt")), "%s upvotes" % pp.get("upvotes", 0)))
    elif t == "json":
        for m in json.loads(body):
            out.append((m["id"], "https://huggingface.co/%s" % m["id"], parse_date(m.get("createdAt")),
                        "%s downloads, %s likes" % (m.get("downloads", 0), m.get("likes", 0))))
    elif t == "sitemap":
        for u in ET.fromstring(body).iter(SITEMAP + "url"):
            loc = (u.findtext(SITEMAP + "loc") or "").strip()
            if not loc.startswith("http"):
                loc = "https://" + loc
            slug = loc.rstrip("/").rsplit("/", 1)[-1].replace("-", " ")
            out.append((slug, loc, parse_date(u.findtext(SITEMAP + "lastmod")), "from sitemap; title is the URL slug"))
    else:  # rss, atom, gnews
        root = ET.fromstring(body)
        if root.tag.endswith("feed"):
            for e in root.findall(ATOM + "entry"):
                link = e.find(ATOM + "link[@rel='alternate']")
                if link is None:
                    link = e.find(ATOM + "link")
                summary = e.findtext(ATOM + "summary") or e.findtext(ATOM + "content") or ""
                out.append((clean(e.findtext(ATOM + "title")), link.get("href") if link is not None else "",
                            parse_date(e.findtext(ATOM + "published") or e.findtext(ATOM + "updated")),
                            clean(summary)[:300]))
        else:
            chan = root.find("channel")
            for i in chan.findall("item") if chan is not None else []:
                title = clean(i.findtext("title"))
                src_el = i.find("source")
                note = clean(i.findtext("description"))[:300]
                if t == "gnews" and src_el is not None and src_el.text:
                    note = "via " + src_el.text.strip()
                    title = re.sub(r"\s+-\s+" + re.escape(src_el.text.strip()) + r"$", "", title)
                out.append((title, (i.findtext("link") or "").strip(),
                            parse_date(i.findtext("pubDate") or i.findtext(DC + "date")), note))
    return out


def fill_url(url, now):
    return (url.replace("{since_24h}", str(int((now - timedelta(hours=24)).timestamp())))
               .replace("{since_48h}", str(int((now - timedelta(hours=48)).timestamp()))))


def run(sources, since, now, cache):
    def one(src):
        t0 = time.time()
        try:
            feed = fill_url(src["url"], now)
            status, body = fetch(feed, cache)
            if status == 304:  # unchanged: reuse the items parsed last time, still windowed
                kept = [i for i in cache.get(feed, {}).get("items", []) if parse_date(i["date"]) >= since]
                return src["id"], {"ok": True, "status": 304, "n": len(kept), "ms": int((time.time() - t0) * 1000)}, kept
            inc = re.compile(src["include"]) if src.get("include") else None
            exc = re.compile(src["exclude"], re.I) if src.get("exclude") else None
            items = []
            for title, url, d, note in parse(src, body, now):
                if not title or not url or d is None or d < since:
                    continue
                if inc and not inc.search(url if src["type"] == "sitemap" else title):
                    continue
                if exc and exc.search(title):
                    continue
                items.append({"source": src["id"], "source_name": src.get("name", src["id"]),
                              "bucket": src.get("bucket"), "title": title, "url": url,
                              "date": d.isoformat(), "note": note})
            items.sort(key=lambda i: i["date"], reverse=True)
            items = items[: src.get("max_items", 10)]
            if feed in cache:
                cache[feed]["items"] = items
            return src["id"], {"ok": True, "status": status, "n": len(items), "ms": int((time.time() - t0) * 1000)}, items
        except Exception as e:  # one bad source must not sink the run
            return src["id"], {"ok": False, "error": ("%s: %s" % (type(e).__name__, e))[:160],
                               "ms": int((time.time() - t0) * 1000)}, []

    with ThreadPoolExecutor(8) as ex:
        return list(ex.map(one, sources))


def main(argv=None):
    ap = argparse.ArgumentParser()
    ap.add_argument("--sources", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--hours", type=float, default=48)
    ap.add_argument("--cache")
    a = ap.parse_args(argv)
    now = datetime.now(timezone.utc)
    since = now - timedelta(hours=a.hours)
    sources = json.load(open(a.sources))
    cache = {}
    if a.cache and os.path.exists(a.cache):
        try:
            cache = json.load(open(a.cache))
        except ValueError:
            cache = {}
    fetched = {fill_url(s["url"], now) for s in sources if isinstance(s, dict) and isinstance(s.get("url"), str)}
    res = run(sources, since, now, cache)
    # Only this run's URLs stay cached: templated URLs ({since_24h}) differ every run and would
    # otherwise pile up, items and all.
    for key in [k for k in cache if k not in fetched]:
        del cache[key]
    items = sorted((i for _, _, got in res for i in got), key=lambda i: i["date"], reverse=True)
    doc = {"fetched": now.isoformat(), "since": since.isoformat(),
           "log": {sid: meta for sid, meta, _ in res}, "items": items}
    tmp = a.out + ".tmp"
    with open(tmp, "w") as f:
        json.dump(doc, f, ensure_ascii=False, indent=1)
    os.replace(tmp, a.out)
    if a.cache:
        with open(a.cache + ".tmp", "w") as f:
            json.dump(cache, f)
        os.replace(a.cache + ".tmp", a.cache)
    failed = [sid for sid, meta, _ in res if not meta["ok"]]
    print("anchors: %d items from %d/%d sources%s" % (
        len(items), len(res) - len(failed), len(res), ("; failed: " + ", ".join(failed)) if failed else ""))
    return 0


if __name__ == "__main__":
    sys.exit(main())
