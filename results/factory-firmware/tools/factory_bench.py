#!/usr/bin/env python3
"""Factory-vs-factory image test (stdlib only). Pi: stock Dragino app. Stick: stock Electronic Cats receiver.
The JPEG is cut into text chunks "<id>:<seq>/<total>:<hex>", sent one stock-app invocation per chunk, and
re-sent (only the missing chunks) for up to --passes passes. Receiver output is parsed from the stick's serial text."""
import argparse, base64, csv, hashlib, json, math, os, re, subprocess, sys, time
ap=argparse.ArgumentParser()
ap.add_argument('--label',required=True); ap.add_argument('--jpeg',required=True); ap.add_argument('--id',default='A')
ap.add_argument('--passes',type=int,default=3); ap.add_argument('--chunk',type=int,default=120)
ap.add_argument('--limit',type=int,default=0,help='send only the first N chunks (smoke test)')
ap.add_argument('--out',default='results/factory-firmware/data'); ap.add_argument('--pi',default='pi')
a=ap.parse_args()
d=os.path.join(a.out,a.label); os.makedirs(d,exist_ok=True)
jpeg=open(a.jpeg,'rb').read(); n=math.ceil(len(jpeg)/a.chunk)
chunks={i:f"{a.id}:{i:03d}/{n:03d}:"+jpeg[i*a.chunk:(i+1)*a.chunk].hex() for i in range(n)}
want=list(range(a.limit if a.limit else n))
def airtime(pl,sf=7,bw=125e3,cr=1,crc=1,h=0,pre=8):
    ts=2**sf/bw; npay=8+max(math.ceil((8*pl-4*sf+28+16*crc-20*h)/(4*sf))*(cr+4),0)
    return (pre+4.25+npay)*ts
pkt_len=max(len(v) for v in chunks.values()); at=airtime(pkt_len)
cap=subprocess.Popen([sys.executable,os.path.join(os.path.dirname(os.path.abspath(__file__)),'stickcap.py'),'--out',os.path.join(d,'stick')])
time.sleep(1.5)
def parse():
    got={}; rows=[]
    p=os.path.join(d,'stick.packets.csv')
    for r in csv.DictReader(open(p)):
        pl=base64.b64decode(r['payload_b64']); rows.append((float(r['t_mono_s']),int(r['rssi_dbm']),pl))
    return rows
t_run=time.monotonic(); passes=[]; seen_before=0
for ps in range(1,a.passes+1):
    lst=[chunks[i] for i in want]; open(os.path.join(d,f'pass{ps}.chunks.txt'),'w').write('\n'.join(lst)+'\n')
    subprocess.run(['scp','-q',os.path.join(d,f'pass{ps}.chunks.txt'),f'{a.pi}:factory-bench/chunks.txt'],check=True)
    n_rows_before=len(parse())
    t0=time.monotonic()
    r=subprocess.run(['ssh',a.pi,'sh factory-bench/factory-send.sh factory-bench/chunks.txt'],capture_output=True,text=True)
    t1=time.monotonic(); time.sleep(2.0)
    open(os.path.join(d,f'pass{ps}.pi-tx.log'),'w').write(r.stdout)
    rows=parse(); new=rows[n_rows_before:]
    got={}; valid=0; bad=0; rssi=[]
    for tm,rs,pl in rows:
        m=re.fullmatch(rb"%s:(\d{3})/(\d{3}):([0-9a-f]*)"%a.id.encode(),pl)
        if m and int(m.group(2))==n and len(m.group(3))%2==0:
            got.setdefault(int(m.group(1)),bytes.fromhex(m.group(3).decode()))
    this=[]; 
    for tm,rs,pl in new:
        m=re.fullmatch(rb"%s:(\d{3})/(\d{3}):([0-9a-f]*)"%a.id.encode(),pl)
        if m: this.append(int(m.group(1))); rssi.append(rs)
        else: bad+=1
    sent=len(want); recv_unique_this=len(set(this)&set(want)); recv_total=len(this)
    passes.append(dict(pass_no=ps,sent=sent,received_packets=recv_total,received_unique=recv_unique_this,malformed=bad,
        loss_pct=round(100*(sent-recv_unique_this)/sent,2),tx_wall_s=round(t1-t0,2),
        rssi_min=min(rssi) if rssi else None,rssi_max=max(rssi) if rssi else None,rssi_mean=round(sum(rssi)/len(rssi),1) if rssi else None))
    want=[i for i in range(a.limit if a.limit else n) if i not in got]
    t_done=time.monotonic()-t_run
    print(json.dumps(passes[-1]),flush=True)
    if not want: break
    time.sleep(1)
cap.terminate(); cap.wait()
complete=len(got)==n and not want
img=b''.join(got[i] for i in range(n)) if len(got)==n else None
ok=img==jpeg if img else False
if img: open(os.path.join(d,'reassembled.jpg'),'wb').write(img)
elif got: # partial: write what we have as chunks
    open(os.path.join(d,'partial.bin'),'wb').write(b''.join(got.get(i,b'') for i in range(n)))
tx_wall=sum(p['tx_wall_s'] for p in passes)
# Same window as lora2015-log's seconds_to_image: first chunk heard to the chunk that completed
# the picture, plus one packet of airtime. end_to_end_s also counts setup and the fixed waits.
first_t=done_t=None; held=set()
for tm,rs,pl in parse():
    m=re.fullmatch(rb"%s:(\d{3})/(\d{3}):([0-9a-f]*)"%a.id.encode(),pl)
    if not m or int(m.group(2))!=n: continue
    first_t=tm if first_t is None else first_t; held.add(int(m.group(1)))
    if len(held)==n: done_t=tm; break
window=round(done_t-first_t+at,2) if done_t is not None else None
res=dict(label=a.label,jpeg=os.path.basename(a.jpeg),jpeg_bytes=len(jpeg),jpeg_sha256=hashlib.sha256(jpeg).hexdigest(),
  chunks_total=n,chunks_attempted_pass1=passes[0]['sent'],payload_bytes_per_packet_max=pkt_len,
  calc_airtime_per_packet_ms=round(at*1000,1),passes=passes,
  total_packets_sent=sum(p['sent'] for p in passes),
  pass1_loss_pct=passes[0]['loss_pct'],reassembled=complete,matches_original=ok,
  end_to_end_s=round(t_done,2),first_heard_to_complete_s=window,tx_wall_total_s=round(tx_wall,2),
  goodput_bit_s=round(len(jpeg)*8/t_done,1) if complete else None,
  onair_text_rate_bit_s=round(sum(p['sent'] for p in passes)*pkt_len*8/tx_wall,1),
  calc_airtime_total_s=round(at*sum(p['sent'] for p in passes),2))
json.dump(res,open(os.path.join(d,'summary.json'),'w'),indent=1); print(json.dumps(res,indent=1))
