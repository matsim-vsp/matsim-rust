import ast, csv, os, re, subprocess, sys
from pathlib import Path
people = ["11460713", "10044972", "10100827", "10025739", "10064468", "10048703", "100278", "10011254", "10027967", "10020806", "10022322"]
results = []
for person in people:
    env = dict(os.environ, MATSIM_RUST_BKK_PERSON=person, MATSIM_RUST_BKK_CONFIG="/home/varameth/Workspaces/SILO_Simulation/BKK_CASE/analysis/baseline_0p1pct_20261016/config_rust_0p1pct.yml")
    path = Path(f".audit/issue-114-raw-person-{person}-final.txt")
    with path.open("w") as log:
        run = subprocess.run([sys.argv[1], "simulation::replanning::routing::route_proposal_tests::raw_bangkok_pt_journey_stays_within_one_second", "--ignored", "--exact", "--nocapture", "--test-threads=1"], env=env, stdout=log, stderr=subprocess.STDOUT, timeout=120)
    match = re.search(r"raw PT TripRouter timings: (\[.*?\]);", path.read_text())
    times = ast.literal_eval(match.group(1)) if match else []
    results.append([person, run.returncode, *times])
    print(person, "exit", run.returncode, "worst", max(times) if times else "unmeasured", flush=True)
with Path(".audit/issue-114-raw-sampled-final.csv").open("w") as output:
    writer = csv.writer(output)
    writer.writerow(["person", "exit_code", "seconds_1", "seconds_2", "seconds_3", "seconds_4", "seconds_5"])
    writer.writerows(results)
assert all(row[1] == 0 for row in results), "raw-query failures recorded in the audit CSV"
