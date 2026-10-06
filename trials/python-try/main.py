import mido

INPUT = next(
    name for name in mido.get_input_names()
    if "MPK mini 3" in name
)

OUTPUT = next(
    name for name in mido.get_output_names()
    if "Midi Through" in name
)

print("Input :", INPUT)
print("Output:", OUTPUT)

with mido.open_input(INPUT) as inp, mido.open_output(OUTPUT) as out:
    try:
        for msg in inp:
            if msg.type in ("note_on", "note_off"):
                new_note = max(0, msg.note - 12)
                msg = msg.copy(note=new_note)

            out.send(msg)
            print(msg)

    except KeyboardInterrupt:
        print("\nStopped.")