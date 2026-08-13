package devices

// The words a fingerprint is rendered in, and the constant that says they have
// not changed.
//
// # Why this list exists twice
//
// It is Esper's own list, frozen, and it lives in `esper-crypto` as well. Two
// copies of a frozen list can drift, so they are held to each other the way the
// wire fixtures are: both sides compute the same FNV-1a over the same words and
// assert the same constant. Change a word in either place and both test suites
// fail.
//
// The alternative was for this side to show hex instead. That loses the reason
// the word form exists — hex is what a person's eye slides over, and eight
// words read as four and four is a code somebody actually checks — and the
// device screen is exactly where that comparison happens.
//
// The six-per-line layout is pinned for the same reason it is pinned in Rust:
// for a frozen list, a one-line-per-change diff is worth the annotation.

// FingerprintDomain is what the fingerprint digest is for, mixed in before the
// key so it can never collide with another use of SHA-256 over the same key.
// It is `esper_crypto::fingerprint::DOMAIN`, and a gate holds the two equal.
const FingerprintDomain = "esper-device-fingerprint-v1"

// FingerprintWords is how many words a rendered fingerprint has. Eight words
// of eight bits is 64, which is the bar for a value a human compares.
const FingerprintWords = 8

// WordlistChecksum is FNV-1a over every word, each followed by 0xff. It is the
// same value `esper-crypto` asserts, and it is what makes the two copies one
// list.
const WordlistChecksum uint64 = 0xdb0c8e256c38cb9f

//nolint:gochecknoglobals // a frozen constant table; Go has no const arrays
var words = [256]string{
	"absurd", "actor", "adopt", "agree", "album", "almond",
	"amuse", "animal", "anvil", "april", "armor", "aspect",
	"atom", "aurora", "awake", "badge", "bamboo", "basil",
	"beetle", "benign", "bicycle", "blanket", "blossom", "bonus",
	"bracket", "bronze", "budget", "bunker", "bypass", "camel",
	"carbon", "caviar", "chalk", "chorus", "citrus", "clever",
	"cluster", "cocoa", "combat", "coral", "cover", "credit",
	"crystal", "cupcake", "cyclone", "daisy", "dapper", "debate",
	"deliver", "descent", "diamond", "digest", "diploma", "diverse",
	"domain", "double", "drummer", "eagle", "eclipse", "eggplant",
	"elder", "email", "emit", "endorse", "enrich", "envelop",
	"errand", "estate", "exact", "expand", "fabric", "fantasy",
	"fedora", "fiber", "figure", "firefly", "fleet", "fluid",
	"fossil", "frost", "gadget", "garden", "gecko", "geyser",
	"giraffe", "global", "golden", "green", "gulf", "hammer",
	"heather", "hexagon", "highway", "honey", "hotel", "hydrant",
	"igloo", "infant", "inner", "iris", "ivory", "jaguar",
	"jersey", "jockey", "jubilee", "jumbo", "kangaroo", "kelp",
	"kettle", "kilo", "kitchen", "koala", "lamp", "larch",
	"laundry", "layer", "lens", "liberty", "linen", "listen",
	"llama", "lodge", "lounge", "lunar", "magnet", "major",
	"maple", "matrix", "member", "meteor", "mimic", "mission",
	"moment", "mosaic", "muffin", "myrtle", "nature", "nebula",
	"neon", "nest", "newborn", "nitrogen", "noodle", "notice",
	"nugget", "nylon", "obelisk", "oboe", "occupy", "office",
	"omega", "opera", "orbit", "origin", "otter", "oxide",
	"pacific", "palace", "parade", "pause", "pencil", "petal",
	"piano", "pilot", "pivot", "planet", "plum", "polar",
	"poster", "prairie", "probe", "pulley", "puppet", "pyramid",
	"quest", "rabbit", "rainbow", "ranch", "razor", "recipe",
	"region", "render", "reveal", "rifle", "river", "rocket",
	"roof", "round", "rudder", "rumor", "rustic", "sailor",
	"sapling", "savory", "school", "sculpt", "sedan", "senate",
	"serene", "shadow", "shrimp", "signal", "siren", "slate",
	"smooth", "sodium", "sorbet", "spark", "splash", "squad",
	"stitch", "studio", "summer", "surf", "sweater", "syntax",
	"tadpole", "tapestry", "teacup", "terrain", "thorn", "ticket",
	"timber", "toaster", "tomato", "total", "toxic", "triangle",
	"tulip", "tuxedo", "ultra", "update", "urban", "utensil",
	"valley", "variety", "velvet", "vessel", "vibrant", "viking",
	"viper", "vital", "voltage", "wafer", "wander", "watch",
	"welcome", "wheat", "window", "wolf", "worthy", "yacht",
	"yeti", "young", "zephyr", "zinc",
}
