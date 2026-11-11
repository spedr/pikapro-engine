import copy
import json

import numpy as np
import pytest

import pikapro
from conftest import play_random, raises_exactly, teams


def test_formats_and_teams():
    assert pikapro.formats() == [
        "gen9singles",
        "gen9doubles",
        "gen9vgc",
        "gen9ou",
        "gen9randombattle",
        "gen9randomdoublesbattle",
        "championsvgc",
        "championsbss",
    ]
    for f in pikapro.formats():
        text = pikapro.sample_team(f)
        packed = pikapro.pack(text, f)
        assert "\n" not in packed
        assert pikapro.export(packed, f) == pikapro.export(text, f)
        # Packed and exported teams make the same battle.
        a = play_random(pikapro.Battle(f, [text, text], seed=3), pikapro.RandomPolicy(1))
        b = play_random(pikapro.Battle(f, [packed, text], seed=3), pikapro.RandomPolicy(1))
        assert a.log() == b.log()


def test_bad_input():
    t = pikapro.sample_team("gen9vgc")
    with pytest.raises(ValueError, match="unknown format"):
        pikapro.Battle("gen1ou", [t, t])
    with pytest.raises(ValueError, match="p2 team"):
        pikapro.Battle("gen9vgc", [t, "Pikachu @ Nothing\n- Tackle"])
    b = pikapro.Battle("gen9vgc", [t, t])
    with pytest.raises(ValueError):
        b.submit(2, 0)
    with pytest.raises(TypeError):
        b.submit(0, 1.5)


def test_a_rust_panic_is_a_runtime_error():
    """A panic in Rust (an engine bug) reaches Python as a RuntimeError,
    which ``except Exception`` catches (PyO3's PanicException is a
    BaseException), with its message; the module goes on working."""
    with pytest.raises(RuntimeError, match="an engine bug") as e:
        pikapro._pikapro._panic("an engine bug")
    assert isinstance(e.value, Exception) and type(e.value).__name__ == "PanicException"
    try:
        pikapro._pikapro._panic("again")
    except Exception:
        pass
    assert pikapro.sample_team("gen9vgc")


def test_requests(format_id):
    b = pikapro.Battle(format_id, teams(format_id), seed=1)
    policy = pikapro.RandomPolicy(2)
    while not b.over:
        for side in (0, 1):
            kind = b.request_kind(side)
            req = b.request(side)
            data = json.loads(b.request_json(side))
            assert req._variant == "RequestData"
            # Fields always win: `kind` is RequestData's field.
            assert pikapro.kind(req.kind) == pikapro.kind(req["kind"])
            assert len(req.pokemon) == len(data["side"]["pokemon"])
            assert req.pokemon[0].species in data["side"]["pokemon"][0]["details"]
            expected = {"wait": "Wait", "team_preview": "TeamPreview", "move": "Move", "force_switch": "ForceSwitch"}
            assert pikapro.kind(req["kind"]) == expected[kind]
            if kind == "wait":
                assert data.get("wait") is True
                assert b.legal_actions(side) == []
            c = policy.choose(b, side)
            if c is not None:
                b.submit(side, c)


def test_submit_forms(format_id):
    """An index, per-slot indices, a string and a Choice are the same
    choice."""
    for form in ("index", "slots", "str", "choice"):
        b = pikapro.Battle(format_id, teams(format_id), seed=4)
        ref = pikapro.Battle(format_id, teams(format_id), seed=4)
        policy = pikapro.RandomPolicy(8)
        while not ref.over:
            for side in (0, 1):
                c = policy.choose(ref, side)
                if c is None:
                    continue
                if c.kind == "team" and ref.space.lead_only:
                    # Team preview brings the whole team (gen9ou): the
                    # space has an index for each lead, with the others in
                    # team order, and none for the order drawn.
                    with pytest.raises(ValueError):
                        ref.choice_index(side, c)
                    c = ref.space.decode(ref.space.joint_size + c.order[0])
                i = ref.choice_index(side, c)
                assert i in ref.legal_actions(side)
                if form == "index":
                    arg = i
                elif form == "slots" and c.kind == "actions":
                    arg = ref.space.split(i)
                elif form == "str":
                    arg = str(c)
                else:
                    arg = c
                ref.submit(side, c)
                b.submit(side, arg)
        assert b.log() == ref.log()


def test_rejected_choice_changes_nothing():
    t = pikapro.sample_team("gen9singles")
    b = pikapro.Battle("gen9singles", [t, t], seed=1)
    before = (b.log(), b.state())
    illegal = [i for i in range(b.space.size) if i not in b.legal_actions(0)]
    with pytest.raises(ValueError, match="rejected"):
        b.submit(0, illegal[0])
    with pytest.raises(ValueError):
        b.submit(0, "move 9")
    assert (b.log(), b.state()) == before
    first = b.legal_actions(0)[0]
    b.submit(0, "move 1")
    assert b.submit(0, "move 1") is False
    assert b.submit(0, first) is False


def test_fork():
    t = pikapro.sample_team("gen9vgc")
    b = pikapro.Battle("gen9vgc", [t, t], seed=6)
    play = lambda battle: play_random(battle, pikapro.RandomPolicy(3))  # noqa: E731
    b.submit(0, "team 1234")
    b.submit(1, "team 4321")
    same = b.fork()
    also = copy.deepcopy(b)
    reseeded = b.fork(seed=123)
    assert reseeded.seed == 123
    assert play(b).log() == play(same).log() == play(also).log()
    # The fork is independent of the original.
    assert same.event_count == b.event_count
    showdown = pikapro.Battle("gen9vgc", [t, t], seed=(1, 2, 3, 4))
    assert showdown.seed == (1, 2, 3, 4)


def test_sodium_seed():
    # Showdown's default PRNG (`sodium`): a 32-byte ChaCha20 key.
    key = bytes(range(32))
    t = pikapro.sample_team("gen9vgc")
    a = play_random(pikapro.Battle("gen9vgc", [t, t], seed=key), pikapro.RandomPolicy(1))
    b = play_random(pikapro.Battle("gen9vgc", [t, t], seed=key), pikapro.RandomPolicy(1))
    assert a.seed == key
    assert a.log() == b.log()
    other = play_random(pikapro.Battle("gen9vgc", [t, t], seed=bytes(32)), pikapro.RandomPolicy(1))
    assert other.log() != a.log()
    with pytest.raises(TypeError):
        pikapro.Battle("gen9vgc", [t, t], seed=bytes(31))


def test_events_are_typed(format_id):
    b = play_random(pikapro.Battle(format_id, teams(format_id), seed=2), pikapro.RandomPolicy(5))
    events = b.events()
    assert len(events) == b.event_count
    assert events[0]._variant == "Player"
    kinds = {e._variant if isinstance(e, pikapro.Record) else e for e in events}
    assert {"Player", "Turn", "Move", "Damage"} <= kinds
    moves = [e for e in events if isinstance(e, pikapro.Record) and e._variant == "Move"]
    names = set(pikapro.names("moves"))
    for m in moves:
        assert m.move_id in names
        assert m.user.pos.side in ("P1", "P2")
        assert m.user.species in pikapro.names("species")
    dmg = next(e for e in events if isinstance(e, pikapro.Record) and e._variant == "Damage")
    assert 0 <= dmg.hp <= dmg.max_hp
    d = dmg._to_dict()
    assert d["_variant"] == "Damage" and isinstance(d["target"], dict)
    assert dmg._keys() == [k for k in d if k != "_variant"]
    assert events[3:] == b.events(3)
    assert "hp" in dmg and dmg["hp"] == dmg.hp


def test_log_perspectives():
    # championsvgc mirrors a ladder format; the custom-game presets report
    # exact HP to everyone, as Showdown's custom games do.
    t = pikapro.sample_team("championsvgc")
    b = play_random(pikapro.Battle("championsvgc", [t, t], seed=11), pikapro.RandomPolicy(12))
    raw = b.raw_log()
    assert any(line.startswith("|split|") for line in raw)
    omni = b.log()
    p1 = b.log("p1")
    spec = b.log("spectator")
    assert b.log(0) == p1 and b.log("omniscient") == omni
    assert len(omni) >= len(p1) and len(omni) >= len(spec)
    # The spectator sees percentages for everyone; p1 its own exact HP.
    assert any("|-damage|p1" in line and "/100" in line for line in spec)
    assert not any("|-damage|p1" in line and "/100" in line for line in p1)
    assert any("|-damage|p2" in line and "/100" in line for line in p1)
    with pytest.raises(ValueError):
        b.log("p3")


def test_state_and_arrays(format_id):
    b = pikapro.Battle(format_id, teams(format_id), seed=1)
    play_random(b, pikapro.RandomPolicy(1))
    state = b.state()
    pokemon, sides, field = b.arrays()
    assert pokemon.dtype == np.int32 and pokemon.shape == (2, 6, len(pikapro.POKEMON_COLUMNS))
    assert sides.shape == (2, len(pikapro.SIDE_COLUMNS)) and field.shape == (len(pikapro.FIELD_COLUMNS),)
    hp = pikapro.column("hp")
    species = pikapro.column("species")
    for s in range(2):
        team = state.sides[s].team
        for pos, mon in enumerate(team):
            assert pokemon[s, pos, hp] == mon.hp
            assert pikapro.names("species")[pokemon[s, pos, species]] == mon.species
        assert (pokemon[s, len(team):] == -1).all()
    assert field[pikapro.column("turn", pikapro.FIELD_COLUMNS)] == b.turn == state.turn
    assert field[pikapro.column("winner", pikapro.FIELD_COLUMNS)] == (b.winner if b.winner is not None else 2)


def test_choice_input_is_strict():
    fmt = "gen9doubles"
    b = pikapro.Battle(fmt, teams(fmt), seed=1)
    s = b.space
    legal = b.legal_actions(0)
    with pytest.raises(TypeError, match="not a bool"):
        b.submit(0, True)
    with pytest.raises(TypeError, match="not a bool"):
        b.choice_index(0, np.bool_(True))
    with pytest.raises(IndexError, match="choice -1 out of range"):
        b.submit(0, -1)
    with pytest.raises(IndexError, match="out of range"):
        b.submit(0, s.size)
    with pytest.raises(IndexError, match="out of range"):
        b.submit(0, 2**70)
    # A numpy uint64 past i64::MAX is an int out of range too, not "no int".
    with raises_exactly(IndexError, "a choice index out of range"):
        b.submit(0, np.uint64(2**63))
    with pytest.raises(TypeError):
        b.submit(0, float(legal[0]))
    first, second = s.split(legal[0])
    with pytest.raises(TypeError, match="not a bool"):
        b.submit(0, [first, False])
    with pytest.raises(IndexError, match="slot action -1"):
        b.submit(0, [first, -1])
    with raises_exactly(IndexError, "a slot action out of range"):
        b.submit(0, [first, np.uint64(2**64 - 1)])
    with pytest.raises(ValueError, match="2 slot actions, got 1"):
        b.submit(0, [first])
    assert b.request_kind(0) == "move"
    # numpy ints are ints.
    assert b.choice_index(0, np.int64(legal[0])) == legal[0]
    assert b.choice_index(0, [np.int32(first), np.uint8(second)]) == legal[0]
    b.submit(0, np.int64(legal[0]))
    assert b.request_kind(0) == "wait"


def test_answered_request_is_flagged(format_id):
    b = pikapro.Battle(format_id, teams(format_id), seed=1)
    before = b.request(0)
    assert before["answered"] is False
    b.submit(0, b.legal_actions(0)[0])
    after = b.request(0)
    # The request still describes what was answered, and says so.
    assert b.request_kind(0) == "wait" and b.legal_actions(0) == []
    assert after["answered"] is True and pikapro.kind(after["kind"]) == pikapro.kind(before["kind"])
    assert b.request(1)["answered"] is False


def test_player_names():
    t = pikapro.sample_team("gen9singles")
    b = pikapro.Battle("gen9singles", [t, t], seed=1, players=("Alice", "Bob"))
    assert b.players == ["Alice", "Bob"]
    assert "|player|p1|Alice|" in "\n".join(b.log("p1"))
    b.players = ["Carol", "Dan"]
    assert any(line.startswith("|player|p2|Dan|") for line in b.log("spectator"))


TRAPPED = [
    "Pikachu\nAbility: Static\n- Tackle\n\nSnorlax\nAbility: Thick Fat\n- Tackle\n",
    "Wobbuffet\nAbility: Shadow Tag\n- Splash\n",
]


def test_an_unavailable_choice_updates_the_request():
    """A switch the request allows (``maybeTrapped``) but a foe's unseen
    Shadow Tag forbids: Showdown's ``[Unavailable choice]``, then the
    request again with ``trapped`` and ``update: true``."""
    b = pikapro.Battle("gen9singles", TRAPPED, seed=4_600_000)
    b.submit(0, "move 1")
    b.submit(1, "move 1")
    req = json.loads(b.request_json(0))
    assert req["active"][0].get("maybeTrapped") and "update" not in req
    with pytest.raises(ValueError, match=r"^\[Unavailable choice\] Can't switch: The active Pok"):
        b.submit(0, "switch 2")
    req = json.loads(b.request_json(0))
    assert req["active"][0]["trapped"] is True and "maybeTrapped" not in req["active"][0]
    assert req["update"] is True
    assert b.request(0).kind.active[0].trapped is True
    with pytest.raises(ValueError, match=r"^\[Invalid choice\]"):
        b.submit(0, "switch 2")
    b.submit(0, "move 1")
    b.submit(1, "move 1")
    assert "update" not in json.loads(b.request_json(0))


def test_an_unavailable_choice_by_index():
    b = pikapro.Battle("gen9singles", TRAPPED, seed=4_600_000)
    b.submit(0, 0)
    b.submit(1, 0)
    switch = b.space.switch_base + 1
    with pytest.raises(ValueError, match="Unavailable choice"):
        b.submit(0, switch)
    assert json.loads(b.request_json(0))["active"][0]["trapped"] is True


def test_open_team_sheets():
    t = pikapro.sample_team("championsvgc")
    b = pikapro.Battle("championsvgc", [t, t], seed=1)
    p1, p2 = b.open_team_sheets()
    assert p1.startswith("|showteam|p1|") and p2.startswith("|showteam|p2|")
    assert "Incineroar" in p1
    b.submit(0, "team 1234")
    b.submit(1, "team 1234")
    with pytest.raises(ValueError):
        b.open_team_sheets()  # after team preview

