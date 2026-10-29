# v2.2 prelaunch schema erratum

v2.1 fixed the required primary-read declaration but failed to load the split federation catalogue: the supply graph retained Calibration without any owning capability. v2.2 removes orphan entity definitions from the split supply and lab graphs. Their semantic values and explicit prerequisite contract/binding remain. No capability, description, input, output, request, label, expected path, budget or rubric changes. Native schema validation requires each declared entity to have a capability.

Both earlier oracle runs are preserved; neither reached a provider. The original v2 generator is provenance only; this version is produced by repair.py from v2.1. Cases and label rationales remain byte-identical across all three versions. This is a catalogue validity correction, not tuning measured ranking or model behavior.
