# Swap a model (D11)

Callers ask for a role (`pmos/default`, `pmos/extraction`), never a model. Swapping a model
changes only `deploy/.env` and `deploy/gateway.yaml`, and then has to pass the benchmarks.

1. **Choose and check.** Is the new model open-weight, licensed for this use, and does it fit in
   the GPU share next to the other model?
2. **Point the model server at the new model.** In `deploy/.env`, change
   `PMOS_MODEL_DEFAULT` (or `PMOS_MODEL_EXTRACTION`) to its Hugging Face id. If it needs a
   different memory share, change `PMOS_MODEL_*_GPU_SHARE` too.
3. **Update the gateway.**
   - In `deploy/gateway.yaml`, set the role's `model:` to the new served name.
   - In `deploy/compose.yaml`, also change `--served-model-name` for that server if the name
     changes.
   - Leave `workarounds:` as they are for now: step 5 tells you whether they're still needed.
4. **Restart both:**
   ```sh
   deploy/pmos-admin up --models
   deploy/pmos-admin logs vllm-default -f      # wait for the model to load
   deploy/pmos-admin restart gateway
   deploy/pmos-admin smoke <your handle>
   ```
5. **Run the benchmarks** for every category that uses the role:
   ```sh
   deploy/pmos-admin evals run --category docs-qa --as <you> --note "model swap: <model>"
   ```
   - **If it misses the bar,** swap back: undo steps 2–3 and restart.
   - **To retire the workarounds** (D11), rerun with them off: set
     `PMOS_GATEWAY_WORKAROUNDS=off` on the gateway for one run. If the scores hold, delete the
     workaround from the role.
6. **Commit the reports** and record the swap in `CHANGELOG.md`.
