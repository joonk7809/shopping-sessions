use crate::pq;

pub fn write_all(outdir: &std::path::Path, urs: &[&crate::UserRows], cats: &[Vec<u8>], tot: &[[u64; 4]], cname: &[Vec<u8>], cchan: &[Vec<u8>], ndays: usize, day0: i64, organic: usize) {
    let ud = outdir.join("user_daily");
    let cd = outdir.join("campaign_daily");
    std::fs::create_dir_all(&ud).unwrap();
    std::fs::create_dir_all(&cd).unwrap();
    let cr: Vec<&[u8]> = cats.iter().map(|c| c.as_slice()).collect();
    std::thread::scope(|sc| {
        for (t, u) in urs.iter().enumerate() {
            let (ud, cr) = (&ud, &cr);
            sc.spawn(move || {
                let x = pq::UserDaily {
                    user_id: pq::Strs { off: &u.uoff, dat: &u.uid },
                    day_base: 0,
                    day: pq::Ints::I32(&u.day),
                    sessions: pq::Ints::I64(&u.sessions),
                    events: pq::Ints::I64(&u.events),
                    revenue_cents: pq::Ints::I64(&u.revenue),
                    top_category: &u.top,
                    categories: cr,
                };
                pq::user_daily(ud, &format!("p{t}_"), &x, u.day.len(), 1).unwrap();
            });
        }
        let mut coff = vec![0u32];
        let mut cdat = Vec::new();
        let mut hoff = vec![0u32];
        let mut hdat = Vec::new();
        let mut day = Vec::new();
        let mut v = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
        for c in 0..cname.len() {
            for d in 0..ndays {
                let r = tot[c * ndays + d];
                if r[0] > 0 || (r[3] > 0 && c != organic) {
                    cdat.extend_from_slice(&cname[c]);
                    coff.push(cdat.len() as u32);
                    hdat.extend_from_slice(&cchan[c]);
                    hoff.push(hdat.len() as u32);
                    day.push(d as u32);
                    for z in 0..4 {
                        v[z].push(if z == 3 && c == organic { 0 } else { r[z] as i64 });
                    }
                }
            }
        }
        let x = pq::CampaignDaily {
            campaign_id: pq::Strs { off: &coff, dat: &cdat },
            channel: pq::Strs { off: &hoff, dat: &hdat },
            day_base: day0 as i32,
            day: pq::Ints::U32(&day),
            sessions: pq::Ints::I64(&v[0]),
            purchasing_sessions: pq::Ints::I64(&v[1]),
            revenue_cents: pq::Ints::I64(&v[2]),
            assisted_sessions: pq::Ints::I64(&v[3]),
        };
        pq::campaign_daily(&cd, "c", &x, day.len(), 1).unwrap();
    });
}
