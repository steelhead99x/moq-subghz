#!/usr/bin/env python3
"""Turn the raw runs of scripts/bench-051.sh into the tables of results/stick-0.5.1/ (stdlib only).
usage: analyze-051.py RUNS_DIR OUT_DIR
Writes OUT_DIR/{fsk.csv,lora.csv,lora-summary.csv,compare.csv} and two SVG charts.
Everything computed here from files is "measured". Columns ending _calc are the datasheet formula."""
import csv, glob, json, os, re, statistics as st, subprocess, sys

RUNS, OUT = sys.argv[1], sys.argv[2]
HERE = os.path.dirname(os.path.abspath(__file__))
FRAMES = os.path.join(HERE, '..', 'results', 'lora-2015', 'frames')
FACTORY = os.path.join(HERE, '..', 'results', 'factory-firmware', 'results.csv')
FACTORY_DATA = os.path.join(HERE, '..', 'results', 'factory-firmware', 'data')
os.makedirs(OUT, exist_ok=True)

def num(x):
    try: return float(x)
    except (TypeError, ValueError): return None

def mean(v): v = [x for x in v if x is not None]; return round(sum(v) / len(v), 2) if v else None
def rng(v): v = [x for x in v if x is not None]; return (min(v), max(v)) if v else (None, None)

# ---------------- FSK live video ----------------
fsk = []
for d in sorted(glob.glob(os.path.join(RUNS, 'f300-*'))):
    if not os.path.isdir(d): continue
    try: s = json.load(open(os.path.join(d, 'summary.json')))
    except Exception: continue
    if not s.get('radio_frames_ok'): continue          # run did not happen (sender failed)
    try: t = json.load(open(os.path.join(d, 'send_summary.json')))
    except Exception: t = {}
    n = os.path.basename(d)
    fsk.append({
        'run': n, 'fec_k': t.get('fec_k'), 'power_dbm': t.get('power_dbm'), 'cipher': s.get('cipher'),
        'span_s': s['span_s'], 'sender_radio_busy_pct': t.get('radio_busy_pct'), 'sender_air_kbit_s': round(t['air_bps']/1000, 1) if t.get('air_bps') else None,
        'radio_frames_ok': s['radio_frames_ok'], 'radio_frames_missing': s['radio_frames_missing'], 'radio_frames_bad_crc': s['radio_frames_bad'],
        'radio_frame_loss_pct': round(100 * s['radio_frame_loss'], 3), 'fec_recovered': s['fec_recovered'],
        'objects_lost': s['objects_lost'], 'groups_cut': s['groups_cut'], 'video_frame_loss_pct': round(100 * s['video_frame_loss'], 2) if s['video_frame_loss'] is not None else None,
        'delivered_video_kbit_s': round(s['delivered_video_bps']/1000, 1), 'radio_goodput_kbit_s': round(s['radio_goodput_bps']/1000, 1),
        'obj_lat_p50_ms': s['obj_lat_ms']['p50'], 'obj_lat_p95_ms': s['obj_lat_ms']['p95'],
        'g2g_p50_ms': s['g2g_lat_ms']['p50'], 'g2g_p95_ms': s['g2g_lat_ms']['p95'], 'g2g_max_ms': s['g2g_lat_ms']['max'],
        'stalls_over_250ms': s['stalls_decoded'], 'worst_gap_ms': s['worst_gap_decoded_ms'], 'decoded_frames': s['decoded_frames'],
        'rssi_mean_dbm': s['rssi_mean'], 'clock_offset_ms': s['clock_offset_ms'], 'auth_fail': s.get('auth_fail'),
    })
if fsk:
    with open(os.path.join(OUT, 'fsk.csv'), 'w', newline='') as f:
        w = csv.DictWriter(f, fieldnames=list(fsk[0])); w.writeheader(); w.writerows(fsk)

# ---------------- lora2015 ----------------
def psnr(rebuilt, ref, crop=None):
    if not os.path.exists(rebuilt): return None
    flt = '[0:v][1:v]psnr' if not crop else f'[1:v]crop={crop}[r];[0:v][r]psnr'
    try:
        r = subprocess.run(['ffmpeg', '-v', 'info', '-i', rebuilt, '-i', ref, '-lavfi', flt, '-f', 'null', '-'], capture_output=True, text=True)
        m = re.search(r'average:(\S+)', r.stderr)
        return None if not m or m.group(1) == 'inf' else round(float(m.group(1)), 1)
    except Exception: return None

def passes(rx):
    """Split received frames into passes where the chunk index does not go up (a pass always sends 0, 1, 2 ...)."""
    out, cur, last_i, last_t = [], [], None, None
    for r in rx:
        i, t = int(r['index']), float(r['host_s'])
        if cur and i <= last_i: out.append(cur); cur = []
        cur.append(i); last_i, last_t = i, t
    if cur: out.append(cur)
    return out

lora = []
for d in sorted(glob.glob(os.path.join(RUNS, '*-r[0-9]'))):
    n = os.path.basename(d)
    m = re.match(r'(chunk|ssdv|lorawan)-(sf\d-\d+)-(small|large)-r(\d+)$', n)
    if not m or not os.path.exists(os.path.join(d, 'exact', 'images.csv')): continue
    mode, prof, size, rep = m.group(1), m.group(2), m.group(3), int(m.group(4))
    im = list(csv.DictReader(open(os.path.join(d, 'exact', 'images.csv'))))
    if not im: continue
    im = im[0]
    rx = [r for r in csv.DictReader(open(os.path.join(d, 'exact', 'rx.csv'))) if r['status'] in ('new', 'dup')]
    bad = [r for r in csv.DictReader(open(os.path.join(d, 'exact', 'rx.csv'))) if r['status'] not in ('new', 'dup')]
    send = open(os.path.join(RUNS, n + '.send')).read()
    sent = int(re.search(r'sent frames=(\d+)', send).group(1)); wall = float(re.search(r'wall_s=([\d.]+)', send).group(1))
    expect = int(im['frames_expected'])
    ps = passes(rx)
    p1 = len(set(ps[0])) if ps else 0
    seen, done_at = set(), None
    for k, p in enumerate(ps, 1):
        seen |= set(p)
        if len(seen) >= expect and done_at is None: done_at = k
    rssi = [num(r['rssi_dbm']) for r in rx]; snr = [num(r['snr_db']) for r in rx]
    jpg = [x for x in glob.glob(os.path.join(d, 'exact', 'image*.jpg'))]
    jpg_bytes = os.path.getsize(jpg[0]) if jpg else None
    secs = num(im['seconds_to_image'])
    ref = {('small', 'chunk'): 'f160x120-q20.jpg', ('small', 'lorawan'): 'f160x120-q20.jpg', ('large', 'chunk'): 'f320x240-q10.jpg', ('large', 'lorawan'): 'f320x240-q10.jpg'}.get((size, mode))
    ps_db = None
    if mode == 'ssdv' and jpg:
        ps_db = psnr(jpg[0], os.path.join(FRAMES, 'f320x240-q10.jpg')) if size == 'large' else psnr(jpg[0], os.path.join(FRAMES, 'f160x120-q20.jpg'), '160:112:0:0')
    lora.append({
        'run': n, 'mode': mode, 'profile': prof, 'size': size, 'rep': rep, 'frames_sent': sent, 'frames_expected': expect,
        'frames_heard': len(rx), 'frames_bad': len(bad), 'loss_exact_pct': num(im['loss_exact_pct']),
        'pass1_heard': p1, 'pass1_loss_pct': round(100 * (expect - p1) / expect, 1), 'passes_to_complete': done_at,
        'complete': int(im['complete']), 'byte_identical': im['matches_reference'] if mode != 'ssdv' else '',
        'ssdv_psnr_db': ps_db, 'picture_bytes': jpg_bytes, 'seconds_to_image': secs,
        'goodput_bit_s': round(8 * jpg_bytes / secs) if jpg_bytes and secs else None,
        'airtime_per_picture_s_calc': num(im['air_toa_s_calc']), 'best_case_goodput_bit_s_calc': round(8 * jpg_bytes / num(im['air_toa_s_calc'])) if jpg_bytes and num(im['air_toa_s_calc']) else None,
        'sender_wall_s': wall, 'rssi_mean_dbm': mean(rssi), 'rssi_min_dbm': rng(rssi)[0], 'rssi_max_dbm': rng(rssi)[1],
        'snr_mean_db': mean(snr), 'snr_min_db': rng(snr)[0], 'snr_max_db': rng(snr)[1],
    })
if lora:
    with open(os.path.join(OUT, 'lora.csv'), 'w', newline='') as f:
        w = csv.DictWriter(f, fieldnames=list(lora[0])); w.writeheader(); w.writerows(lora)
    groups = {}
    for r in lora: groups.setdefault((r['size'], r['profile'], r['mode']), []).append(r)
    order = {'sf7-125': 0, 'sf7-250': 1, 'sf7-500': 2, 'sf8-500': 3}
    rows = []
    for (size, prof, mode), g in sorted(groups.items(), key=lambda kv: (kv[0][0] != 'small', order[kv[0][1]], kv[0][2])):
        rows.append({'size': size, 'profile': prof, 'mode': mode, 'runs': len(g), 'complete': sum(r['complete'] for r in g),
                     'frames_per_picture': g[0]['frames_expected'], 'loss_exact_pct_mean': mean([r['loss_exact_pct'] for r in g]),
                     'pass1_loss_pct_mean': mean([r['pass1_loss_pct'] for r in g]),
                     'seconds_to_image_mean': mean([r['seconds_to_image'] for r in g]), 'seconds_to_image_min': rng([r['seconds_to_image'] for r in g])[0], 'seconds_to_image_max': rng([r['seconds_to_image'] for r in g])[1],
                     'goodput_bit_s_mean': mean([r['goodput_bit_s'] for r in g]), 'airtime_per_picture_s_calc': g[0]['airtime_per_picture_s_calc'],
                     'rssi_mean_dbm': mean([r['rssi_mean_dbm'] for r in g]), 'snr_mean_db': mean([r['snr_mean_db'] for r in g]), 'ssdv_psnr_db': mean([r['ssdv_psnr_db'] for r in g])})
    with open(os.path.join(OUT, 'lora-summary.csv'), 'w', newline='') as f:
        w = csv.DictWriter(f, fieldnames=list(rows[0])); w.writeheader(); w.writerows(rows)

    # factory vs 0.5.1, same radio settings (SF7 / 125 kHz), same two pictures, timed the same
    # way: first packet of the picture heard to the packet that completes it, plus one
    # packet of airtime (what lora2015-log's seconds_to_image is). The factory script's own
    # end_to_end_s also counts setup, the scp of each pass and a fixed 2 s wait after each
    # pass, so it is kept only as a separate column.
    def factory_window(run, airtime_s):
        pk = re.compile(rb'([A-Za-z]):(\d{3})/(\d{3}):([0-9a-f]*)')
        held, first, key = set(), None, None
        import base64
        for r in csv.DictReader(open(os.path.join(FACTORY_DATA, run, 'stick.packets.csv'))):
            m = pk.fullmatch(base64.b64decode(r['payload_b64']))
            if not m or len(m.group(4)) % 2: continue
            k = (m.group(1), int(m.group(3)))
            if key is None: key = k
            if k != key: continue
            t = float(r['t_mono_s'])
            if first is None: first = t
            held.add(int(m.group(2)))
            if len(held) == key[1]: return round(t - first + airtime_s, 2)
        return None
    fac = list(csv.DictReader(open(FACTORY)))
    for r in fac:
        r['window_s'] = factory_window(r['run'], num(r['calc_airtime_per_packet_ms']) / 1000)
    cmp_rows = []
    for size, tag in (('small', 'small-160x120'), ('large', 'large-320x240')):
        fr = [r for r in fac if r['run'].startswith(tag)]
        if fr:
            cmp_rows.append({'picture': size, 'stack': 'factory (Dragino app + stock stick)', 'runs': len(fr),
                'all_complete': all(r['byte_identical'] == 'True' for r in fr), 'pass1_loss_pct_mean': mean([num(r['pass1_loss_pct']) for r in fr]),
                'packets_per_picture': fr[0]['chunks'], 'seconds_to_picture_mean': mean([r['window_s'] for r in fr]),
                'seconds_min': rng([r['window_s'] for r in fr])[0], 'seconds_max': rng([r['window_s'] for r in fr])[1],
                'goodput_bit_s_mean': mean([8 * num(r['jpeg_bytes']) / r['window_s'] for r in fr if r['window_s']]),
                'rssi_mean_dbm': mean([(num(r['rssi_min_dbm']) + num(r['rssi_max_dbm'])) / 2 for r in fr]), 'snr_mean_db': '',
                'factory_end_to_end_s_mean': mean([num(r['end_to_end_s']) for r in fr])})
        for mode in ('chunk', 'lorawan', 'ssdv'):
            g = groups.get((size, 'sf7-125', mode))
            if not g: continue
            cmp_rows.append({'picture': size, 'stack': f'moq-subghz 0.5.1 {mode}', 'runs': len(g), 'all_complete': all(r['complete'] for r in g),
                'pass1_loss_pct_mean': mean([r['pass1_loss_pct'] for r in g]), 'packets_per_picture': g[0]['frames_expected'],
                'seconds_to_picture_mean': mean([r['seconds_to_image'] for r in g]), 'seconds_min': rng([r['seconds_to_image'] for r in g])[0], 'seconds_max': rng([r['seconds_to_image'] for r in g])[1],
                'goodput_bit_s_mean': mean([r['goodput_bit_s'] for r in g]), 'rssi_mean_dbm': mean([r['rssi_mean_dbm'] for r in g]), 'snr_mean_db': mean([r['snr_mean_db'] for r in g]),
                'factory_end_to_end_s_mean': ''})
    with open(os.path.join(OUT, 'compare.csv'), 'w', newline='') as f:
        w = csv.DictWriter(f, fieldnames=list(cmp_rows[0])); w.writeheader(); w.writerows(cmp_rows)

    # charts (plain SVG)
    def bars(path, title, items, unit):
        W, H, L = 760, 40 + 30 * len(items), 330
        mx = max(v for _, v, _ in items) or 1
        o = [f'<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{H}" font-family="sans-serif" font-size="13">', f'<rect width="{W}" height="{H}" fill="white"/>', f'<text x="10" y="22" font-size="15" font-weight="bold">{title}</text>']
        for k, (lab, v, col) in enumerate(items):
            y = 40 + 30 * k; w = (W - L - 90) * v / mx
            o += [f'<text x="{L-8}" y="{y+15}" text-anchor="end">{lab}</text>', f'<rect x="{L}" y="{y}" width="{max(w,1):.1f}" height="20" fill="{col}"/>', f'<text x="{L+w+6:.1f}" y="{y+15}">{v:g} {unit}</text>']
        open(path, 'w').write('\n'.join(o + ['</svg>']))
    for size in ('small', 'large'):
        items = [(r['stack'].replace('moq-subghz 0.5.1 ', 'moq-subghz ') + ' SF7/125', r['seconds_to_picture_mean'], '#c0504d' if r['stack'].startswith('factory') else '#4f81bd') for r in cmp_rows if r['picture'] == size]
        for r in rows:
            if r['size'] == size and r['mode'] == 'chunk' and r['profile'] != 'sf7-125':
                items.append((f"moq-subghz chunk {r['profile'].upper().replace('-', '/')}", r['seconds_to_image_mean'], '#9bbb59'))
        pic = '160x120 JPEG (2346 bytes)' if size == 'small' else '320x240 JPEG (8618 bytes)'
        bars(os.path.join(OUT, f'time-to-picture-{size}.svg'), f'Seconds from first packet to a complete {pic}, measured, mean', items, 's')
print(f'fsk runs {len(fsk)}, lora runs {len(lora)}')
