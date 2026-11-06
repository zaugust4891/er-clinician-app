import http from 'k6/http';
import { sleep, fail } from 'k6';
import crypto from 'k6/crypto';
import { Trend, Rate, Counter } from 'k6/metrics';

const base = (__ENV.BASE_URL || '').replace(/\/$/, '');
const mode = __ENV.PROVIDERS || 'mock';
const vus = Number(__ENV.CLINICIANS || (mode === 'real' ? 1 : 10));
const patients = Number(__ENV.PATIENTS || (mode === 'real' ? 3 : 18));
const encounters = Number(__ENV.ENCOUNTERS || 8);
const horizon = Number(__ENV.SHIFT_SECONDS || 3600);
const interval = Number(__ENV.POLL_SECONDS || 10);
if (__ENV.ACK_SYNTHETIC_LOAD !== 'yes' || !/^https?:\/\//.test(base)) fail('Set BASE_URL and ACK_SYNTHETIC_LOAD=yes');
if (!['mock', 'real'].includes(mode) || ![vus,patients,encounters,horizon,interval].every(n => Number.isInteger(n) && n > 0)) fail('Invalid load configuration');
if (mode === 'mock' && !/^http:\/\/(localhost|127\.0\.0\.1)(:\d+)?$/.test(base)) fail('Mock load requires isolated local infrastructure');
const credentials = JSON.parse(open(__ENV.CREDENTIALS_FILE));
const audio = open(__ENV.AUDIO_FILE, 'b'); // Synthetic spoken WAV; the real run must use meaningful speech.
const latency = new Trend('api_latency_ms', true);
const mdmLatency = new Trend('mdm_completion_ms', true);
const bad = new Rate('operation_failed');
const throttled = new Counter('rate_limited');
const dedupe = new Counter('idempotency_violations');
const completed = new Counter('patients_completed');
export const options = {
  scenarios: { shift: { executor: 'per-vu-iterations', vus, iterations: 1, maxDuration: `${horizon + 600}s` } },
  systemTags: ['method', 'status', 'name', 'scenario', 'expected_response'],
  thresholds: {
    api_latency_ms: ['p(95)<300'], operation_failed: ['rate==0'], rate_limited: ['count==0'],
    idempotency_violations: ['count==0'], patients_completed: [`count==${vus * patients}`],
    ...(mode === 'real' ? { mdm_completion_ms: ['p(95)<30000'] } : {}),
  },
};
function id() {
  const b = new Uint8Array(crypto.randomBytes(16)); let t = Date.now();
  for (let i=5;i>=0;i--) { b[i]=t%256; t=Math.floor(t/256); }
  b[6]=(b[6]&15)|112; b[8]=(b[8]&63)|128;
  const h=Array.from(b,x=>x.toString(16).padStart(2,'0')).join('');
  return `${h.slice(0,8)}-${h.slice(8,12)}-${h.slice(12,16)}-${h.slice(16,20)}-${h.slice(20)}`;
}
function req(method, path, body, name, token, status=200) {
  const r=http.request(method,base+path,body === null ? null : JSON.stringify(body),{
    headers:{Authorization:`Bearer ${token}`,'Content-Type':'application/json','User-Agent':'er-note-load/1'},tags:{name},redirects:0,
  });
  if (!path.startsWith('/v1/jobs/')) latency.add(r.timings.duration);
  if(r.status===429) throttled.add(1);
  bad.add(r.status!==status);
  if(r.status!==status) fail(`Operation ${name} rejected (${r.status})`);
  return r.body ? r.json() : null;
}
export function setup() {
  if(credentials.length!==vus) fail('Provide exactly one credential per clinician');
  const seen=new Set();
  for(const c of credentials) { const me=req('GET','/v1/me',null,'identity',c.token); if(seen.has(me.clinician_id)) fail('Clinicians must be distinct'); seen.add(me.clinician_id); }
}
export default function () {
  const token=credentials[__VU-1].token;
  const roster=[]; let lastPoll=0;
  throttled.add(0); dedupe.add(0);
  req('POST','/v1/shifts',null,'start_shift',token);
  for(let p=0;p<patients;p++) { const patient=id(); req('POST','/v1/patients',{patient_id:patient,chief_complaint:'Synthetic load chest pain',display_label:`Load ${p}`},'register',token); roster.push({id:patient,since:0,chunks:[]}); }
  function poll() {
    for(const p of roster) {
      let page;
      do { page=req('GET',`/v1/patients/${p.id}/events?since=${p.since}&limit=500`,null,'events_poll',token); for(const e of page.events) p.since=Math.max(p.since,e.sequence_id); } while(page.has_more);
      for(const chunk of p.chunks) {
        if(!chunk.done) { const s=req('GET',`/v1/audio/${chunk.id}`,null,'audio_poll',token); if(s.status==='failed') fail('Transcription failed'); chunk.done=s.status==='done'; }
      }
    }
    lastPoll=Date.now();
  }
  function wait(seconds) { const end=Date.now()+Math.max(0,seconds)*1000; while(Date.now()<end) { if(Date.now()-lastPoll>=interval*1000) poll(); sleep(Math.min(1,(end-Date.now())/1000)); } }
  function append(p, events) {
    const body={events:events.map(event=>({event_id:id(),event}))};
    const path=`/v1/patients/${p.id}/events`;
    const a=req('POST',path,body,'append_events',token);
    const b=req('POST',path,body,'retry_events',token);
    if(a.results.length !== b.results.length || a.results.some((ack,i) => ack.event_id !== b.results[i].event_id || ack.sequence_id !== b.results[i].sequence_id)) { dedupe.add(1); fail('Event acknowledgments changed on retry'); }
  }
  const started=Date.now();
  for(let cycle=0;cycle<encounters;cycle++) for(let index=0;index<patients;index++) {
    const p=roster[index], eid=id(); const now=()=>new Date().toISOString();
    append(p,[{AudioStarted:{patient_id:p.id,encounter_id:eid,capture_mode:'Ambient',clinical_occurred_at:now()}}]);
    const count=1+(cycle%2);
    for(let n=0;n<count;n++) {
      const chunk=id();
      const intent=req('POST',`/v1/patients/${p.id}/encounters/${eid}/audio`,{chunk_id:chunk,chunk_index:n,is_final:n===count-1,content_type:'audio/wav'},'audio_intent',token);
      const put=http.put(intent.upload_url,audio,{headers:{'Content-Type':'audio/wav'},tags:{name:'direct_upload'},redirects:0});
      bad.add(put.status!==200); if(put.status!==200) fail('Direct upload rejected');
      req('POST',`/v1/audio/${chunk}/complete`,null,'audio_complete',token); p.chunks.push({id:chunk,done:false});
    }
    append(p,[{AudioStopped:{patient_id:p.id,encounter_id:eid,clinical_occurred_at:now()}},
      {DataReviewed:{patient_id:p.id,physician_review:'Synthetic ECG reviewed',clinical_occurred_at:now()}},
      {DiagnosisAssigned:{patient_id:p.id,diagnosis:{id:id(),patient_id:p.id,text:'Synthetic chest pain',icd10_code:null,is_primary:true},clinical_occurred_at:now()}},
      {MedicationRecorded:{patient_id:p.id,medication:{id:id(),patient_id:p.id,name:'Synthetic aspirin',dose:'324 mg',route:'PO',rxnorm_code:null,kind:'AdministeredInEd',administered_at:now(),frequency:'once'},clinical_occurred_at:now()}},
      {ResultReceived:{patient_id:p.id,result:{id:id(),patient_id:p.id,source:'Manual',timestamp:now(),result_type:{Lab:{fhir_id:null,loinc_code:null,display_name:'Synthetic troponin',value:0.01,unit:'ng/mL',reference_range:null,is_abnormal:false}}},clinical_occurred_at:now()}}]);
    const target=started+((cycle*patients+index+1)/(encounters*patients))*horizon*1000;
    wait((target-Date.now())/1000);
  }
  let deadline=Date.now()+300000;
  while(roster.some(p=>p.chunks.some(c=>!c.done))) { if(Date.now()>deadline) fail('Audio drain timeout'); poll(); sleep(interval); }
  for(const p of roster) {
    if (p !== roster[0]) sleep(13); // Remain below the 5/minute MDM admission quota.
    const begin=Date.now(); const job=id();
    req('POST',`/v1/patients/${p.id}/mdm`,{job_id:job},'request_mdm',token,202);
    for(;;) { const s=req('GET',`/v1/jobs/${job}`,null,'job_poll',token); if(s.status==='failed') fail('MDM failed'); if(s.status==='done') break; if(Date.now()-begin>120000) fail('MDM timeout'); sleep(1); }
    mdmLatency.add(Date.now()-begin);
    req('GET',`/v1/patients/${p.id}/note`,null,'note',token); completed.add(1);
  }
}
export function handleSummary(data) { return { [__ENV.SUMMARY_FILE || 'load-summary.json']: JSON.stringify(data,null,2) }; }
