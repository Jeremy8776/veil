# Mobile networks and internet shutdowns

Research note, 19/11/2026. It asks two questions:

1. Can Veil calls work over mobile data (4G and 5G)?
2. What do governments do when they cut or control the internet, and what could Veil still do
   then?

These are separate problems. The first is engineering. The second has hard limits, and Veil
must be honest about them.

## Summary

- **Mobile data is the normal case, not the edge case.** Most people in India, Iran, Russia and
  across Africa reach the internet on a phone. More than 90% of mobile networks put users behind
  carrier-grade NAT.
- **Veil is UDP only today.** Every link is QUIC over UDP. There is no TCP or port 443 fallback
  in any crate. Iran blocked QUIC entirely during its June 2025 shutdown and kept blocking it
  afterwards. China's firewall inspects QUIC since 2024. On those networks Veil does not
  connect at all.
- **Veil's bitrate ladder starts too high.** The smallest video layer is about 0.5 Mbit/s.
  Throttled networks (Kashmir's long 2G period, Afghanistan in 2025) give far less.
- **A total shutdown kills every internet app, Veil included.** What still works is a local
  network with no internet: a phone hotspot or a router in one building. Veil could make calls
  there with small changes.
- **Whitelists are the new pattern.** Russia (57 regions by late 2025) and Iran (2026) let
  through only approved sites. Nothing that is not on the list gets out, whatever its protocol.
- **Phones are the missing piece for reach**, but a phone app is the largest item here. A
  desktop on a phone's hotspot already works over 5G with no code change, and that is the
  first thing to test.

## 1. How governments cut or control access

Access Now counted at least 313 shutdowns in 52 countries in 2025. Myanmar had the most (95).
India had 65, the most of any democracy, down from 116 in 2023 and 84 in 2024.

| Tactic | What it does | Recent examples |
|---|---|---|
| Total shutdown | All internet, sometimes phone and SMS too | Iran June 2025 (about 97% drop) and January 2026; Afghanistan September to October 2025; Bangladesh July 2024 (10 days) |
| Mobile data only | Mobile internet off, home broadband left on | India (most of its 65), Russia from May 2025 (40+ regions at once), Pakistan |
| Speed cut (throttling) | Network stays up but is too slow for video | Kashmir on 2G for months after 2019; Afghanistan told operators to leave only 2G in 2025 |
| Protocol whitelist | Only DNS, HTTP and HTTPS pass; everything else is dropped | Iran's "stealth blackout" in 2025, at one national gateway |
| QUIC or UDP blocking | QUIC dropped outright, or by server name | Iran 2025 onwards (all QUIC); China since April 2024 (by name) |
| Site whitelist | Only approved sites and apps work | Russia from September 2025, 57 regions by late 2025; Iran 2026 |
| App takedowns | Stores told to remove apps | India, July 2026: Google and Apple told to remove BitChat, Briar and Bridgefy during protests; GitHub told to disable BitChat's code |
| Satellite jamming | Starlink made slow or useless | Iran 2026, up to 80% loss in places |

Two points matter most for Veil:

- **The trend is from "off" to "allowed list".** Russia and Iran both moved to whitelists
  because a full shutdown also stops banks, shops and the state itself. A whitelist is cheaper
  to keep on for months.
- **Tools that people install before a shutdown help. Tools they must download during one do
  not.** Stores and GitHub can be told to remove an app, and the download site may be blocked.

## 2. What mobile networks are like

| Fact | Source strength | Effect on Veil |
|---|---|---|
| More than 90% of mobile networks use carrier-grade NAT | Large measurement study | A phone cannot host a call that others dial into. Guests always need a public rendezvous and often a relay |
| Hole punching worked 75% of the time between Android phones on four Dutch 5G networks | Small 2023 experiment | About 1 in 4 mobile pairs needs the relay. Relay capacity matters (ADR 0002) |
| Jio (India's largest network) and T-Mobile US give most phones IPv6, many IPv6 only | Several sources, figures disagree (roughly 77% to 94%) | ADR 0003 (prefer IPv6) is the right call. Whether mobile networks let inbound IPv6 through is not measured anywhere I found |
| Mobile upload is often 5 to 20 Mbit/s even on 5G, and it changes minute by minute | General knowledge | The 4K ladder does not apply. Hd at 3 Mbit/s is the realistic ceiling |
| Phones on a hotspot share the phone's carrier-grade NAT | General knowledge | A laptop on a hotspot behaves like a phone on the network |

ADR 0004 already says its 1 and 5 second dial stages are "not measured on real mobile
networks". This research says they should be.

## 3. Which tactics Veil could survive

| Tactic | Veil today | Veil could | Limit |
|---|---|---|---|
| Mobile data only | Works on home broadband | Nothing to do | People with no broadband are cut off |
| Speed cut to 2G or 3G | Fails: smallest video is 0.5 Mbit/s | Audio-only mode and a tiny video layer | Below about 30 kbit/s even audio struggles |
| QUIC or UDP blocked | Fails | Relay over TLS on TCP port 443 | Slower, and a censor can still block the relay's address |
| Protocol whitelist (HTTPS only) | Fails | Same TCP 443 fallback, made to look like normal HTTPS | Deep inspection can still spot unusual traffic |
| Site whitelist | Fails | Only if a relay sits on an allowed host | Not something Veil can promise |
| Total shutdown | Fails | Calls on a local network with no internet | Only people in Wi-Fi range of each other |
| App takedown | Not on stores yet | Signed releases on GitHub and mirrors; installers people can pass on by USB | GitHub can be ordered to act too |

## 4. Gaps in Veil today

1. **UDP only.** `veil-net` is QUIC on one UDP socket. No crate has a TCP path.
2. **Ladder starts at 0.5 Mbit/s.** No audio-only mode for poor links. The planner lowers
   layers (ARCHITECTURE.md 3.3 item 10) but has nothing below Thumb.
3. **Short invites need the rendezvous.** The short link holds a secret, and the rendezvous
   keeps the signed invite. With no internet, no rendezvous is reachable from outside the
   local network.
4. **No local discovery yet.** "LAN mDNS" is listed as a rendezvous adapter but is not built.
5. **Desktop only.** `android-activity` is already in the dependency tree (through `eframe`
   and `wgpu`), but `veil-media` has no Android camera, encoder or audio path.

## 5. Recommendations, in order

Each step is useful on its own. The first three are small or medium. The last is large.

### 5.1 Test on real mobile networks (small)

Run `veil-probe` between a laptop on a 5G hotspot and a home connection, and between two
hotspots on different carriers. Record: which path won (direct, punched, relay), time to
connect, whether IPv6 came in, and usable bitrate. Use the results to set the ADR 0004 timings.
No code change needed. This also answers "does 5G work?" for desktop users today.

### 5.2 Low-bandwidth mode (medium)

- An **audio-only** choice in the control bar, also chosen by the planner when video cannot fit.
- A **smaller video layer** below Thumb, around 160p at 7 fps and 100 to 150 kbit/s.
- **Opus at 12 to 16 kbit/s** with forward error correction for poor links. Opus stays clear
  down to about 8 kbit/s for speech.
- Show the reason in the call ("Poor connection: audio only") in line with the UI style rule.

This helps everyone on mobile, not only people under throttling. It is the best value item.

### 5.3 Relay over TLS on TCP port 443 (medium)

When QUIC to the relay fails, dial the same relay over TLS on TCP port 443 and carry the same
sealed QUIC packets inside. Nothing changes for security: the relay still sees only
ciphertext, as now. This gets through networks that block UDP or allow only HTTPS.

Keep claims modest. This beats simple blocks. It does not beat a censor who blocks the
relay's address, a site whitelist, or deep inspection aimed at Veil. Veil is a private call
app, not an anti-censorship tool, and TRANSPARENCY.md should say so. Projects that do this as
their main job (Tor's Snowflake, for example) spend years on it.

### 5.4 Calls with no internet (medium)

During a total shutdown, a phone hotspot or a home router still makes a local network. Veil
already carries calls over any network it can reach. It needs:

- **A full offline invite.** The complete signed invite as a QR code or text, with the host's
  local addresses, so no rendezvous is needed.
- **Local discovery** (the planned mDNS adapter), so people on the same network see an open
  call.
- **The host runs the rendezvous on the local network**, which it already does when no node is
  set.

Bluetooth is too slow for video and is not worth adding. Wi-Fi, through a hotspot or router,
is the right medium.

### 5.5 Android app (large)

This is what puts Veil on phones over 5G. `eframe` runs on Android, so the interface is the
smaller part. The work is in `veil-media`: camera capture, an encoder and decoder (Android's
MediaCodec, or OpenH264 as on Linux), and audio (`cpal` has an AAudio back end). Then come
Android's background limits, battery, and store review. Plan it as its own project, after 5.2,
because a phone app with only the 4K-first ladder would give a poor first call.

iOS is harder again and should wait.

## 6. Safety notes for users

- In some countries having these apps is itself a risk. India ordered mesh messengers removed
  in July 2026. Iran punishes owning a Starlink dish.
- The network can see that a person talks to a relay, and when. During a shutdown, any traffic
  that gets out may stand out.
- Install before you need it. Keep the installer on a USB stick to pass on.

TRANSPARENCY.md should carry a short version of this section if 5.3 or 5.4 ships.

## 7. Options for the person, with a default

People know their own situation better than Veil can guess it. So each new behaviour above is
also a choice they can make, with one default marked "Recommended". The default must work for
most people without them reading anything.

There are two separate choices: how much data a call uses, and which way it connects. They
are independent, so they are two controls, not one long list.

### 7.1 Data use

Where: the pre-join screen, and the call's "..." menu so it can change mid-call. Saved on the
device as the last choice.

| Option | What it does | Tooltip |
|---|---|---|
| **Auto** (Recommended) | Today's behaviour plus the planner dropping to Data saver, then Audio only, when the link cannot carry more | Best picture your connection allows. Drops video quality before it drops the call. |
| Data saver | Video capped at the new small layer (about 150 kbit/s), Opus at 16 kbit/s | For mobile data, slow or costly connections. |
| Audio only | No video sent or received, Opus at 12 to 16 kbit/s with error correction | For very slow networks such as 2G. Uses about 1 MB every 10 minutes. |

When Auto steps down, the call says so ("Poor connection: audio only") and offers the way
back. It never steps down silently.

### 7.2 Connection route

Where: Settings > Connection, above the server fields. Saved in `connection.json` as a new
field that defaults to Auto, so older files still load.

| Option | What it does | Tooltip |
|---|---|---|
| **Auto** (Recommended) | Direct, then punched, then relay (ADR 0004), with relay over TCP 443 as a last stage when every UDP path fails | Finds the fastest way through. Works on most home, office and mobile networks. |
| Hide my address | Relay only, so other people never learn your IP address. Already built in `veil-call`, not yet in the app | Others see the relay, not you. Slightly slower. |
| Restricted network | Goes straight to relay over TCP 443, with no UDP tries first | For networks that block video calls or allow only web browsing. Slower. |
| Local only | No internet at all. Finds calls on the same Wi-Fi; invites are QR codes or text with local addresses | For when the internet is down. Works only with people on the same Wi-Fi or hotspot. |

Why Auto is the default: it already includes the TCP 443 stage, so most people on a
restricted network get through without changing anything. "Restricted network" exists for
people who know UDP is blocked: it connects faster, and it does not send UDP tries that a
watchful network could notice.

When Veil sees no internet at all at call start, it offers Local only in the call screen
("No internet. Call people on this Wi-Fi?") rather than making the person find the setting.

### 7.3 What this means for the build order

- 5.2 delivers 7.1 in full.
- "Hide my address" is the cheapest option to ship, since the mode is built. It can go out
  with 5.2.
- 5.3 adds the TCP 443 stage to Auto and the "Restricted network" option.
- 5.4 adds "Local only".

## Sources

- Access Now, [2025 report on internet shutdowns](https://www.accessnow.org/wp-content/uploads/2026/03/KeepItOn-Internet-Shutdowns-2025-Annual-Report.pdf) and [Asia Pacific press release](https://www.accessnow.org/press-release/one-region-half-the-worlds-shutdowns-internet-shutdowns-in-the-asia-pacific-in-2025/)
- [Business Standard: India records 65 shutdowns in 2025](https://www.business-standard.com/amp/india-news/india-records-65-internet-shutdowns-2025-access-now-report-126033100877_1.html)
- [2025 Internet blackout in Iran (Wikipedia)](https://en.wikipedia.org/wiki/2025_Internet_blackout_in_Iran)
- [Iran's Stealth Internet Blackout (arXiv)](https://arxiv.org/html/2507.14183v1)
- [Insights into an Iranian Internet Shutdown (FOCI 2026)](https://www.petsymposium.org/foci/2026/foci-2026-0016.pdf)
- [Kentik: From stealth blackout to whitelisting](https://www.kentik.com/blog/from-stealth-blackout-to-whitelisting-inside-the-iranian-shutdown/)
- [Filterwatch, January 2026](https://filter.watch/english/2026/01/28/network-monitoring-january-2026-from-regional-disuptions-to-total-blackout-and-whitelisted-access/)
- [Bloomberg: How Iran built its system of internet censorship](https://www.bloomberg.com/features/2026-iran-internet/)
- [Rest of World: Iran's shutdown crippled Starlink](https://restofworld.org/2026/iran-starlink-internet-shutdown/)
- [Meduza: Mapping Russia's internet blackouts](https://meduza.io/en/feature/2025/07/07/mapping-russia-s-internet-blackouts)
- [Carnegie: How far will the Kremlin take its internet crackdown?](https://carnegieendowment.org/russia-eurasia/politika/2025/12/russia-internet-restrictions?lang=en)
- [Human Rights Watch: Russia internet shutdowns escalate](https://www.hrw.org/news/2026/03/31/russia-internet-shutdowns-escalate)
- [Re:Russia: Whitelists for dark times](https://re-russia.net/en/analytics/0364/)
- [2024 Bangladesh internet blackouts (Wikipedia)](https://en.wikipedia.org/wiki/2024_Bangladesh_internet_blackouts)
- [Al Jazeera: Afghanistan imposes internet blackout](https://www.aljazeera.com/news/2025/9/30/afghanistan-imposes-internet-blackout-what-has-the-effect-been-so-far)
- [Human rights cost of shutdowns in South Asia](https://www.humanrightsresearch.org/post/the-human-rights-cost-of-internet-shutdowns-in-south-asia)
- [Zohaib et al., SNI-based QUIC censorship of the GFW (USENIX Security 2025)](https://www.usenix.org/conference/usenixsecurity25/presentation/zohaib)
- [Elmenhorst et al., Web censorship measurements of HTTP/3 over QUIC](https://dl.acm.org/doi/pdf/10.1145/3487552.3487836)
- [Mass adoption of NATs: carrier-grade NAT survey and experiments (arXiv)](https://arxiv.org/pdf/2311.04658)
- [A multi-perspective analysis of carrier-grade NAT deployment](https://www.researchgate.net/publication/310822079_A_Multi-perspective_Analysis_of_Carrier-Grade_NAT_Deployment)
- [APNIC: Reliance Jio boosts India's IPv6](https://blog.apnic.net/2017/02/07/reliance-jio-boosts-india-past-20-ipv6-capability/)
- [World IPv6 Launch measurements](https://www.worldipv6launch.org/?p=966)
- [MediaNama: government targets BitChat, Briar, Bridgefy](https://www.medianama.com/2026/07/223-government-targets-bitchat-briar-bridgefy/)
- [Cybernews: Iran sparks interest in Bluetooth messengers](https://cybernews.com/security/iran-sparks-interest-in-bluetooth-based-messengers/)
