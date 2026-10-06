# OpenLW : app AppKit universelle (x86_64 10.13, arm64 11.0).
#   make            construit build/OpenLW.app (signature ad hoc)
#   make check-min  affiche le plancher OS de chaque tranche
#   make snapshot   lance l'app et rend sa fenêtre dans build/snapshot.png
#   make install    copie l'app dans /Applications
#
# Le SDK de Xcode est requis (NSGlassEffectView, macOS 26). Sous 10.14.4, le runtime Swift
# n'est pas fourni par le système : swift-stdlib-tool l'embarque dans Contents/Frameworks.

DEVELOPER_DIR ?= /Applications/Xcode.app/Contents/Developer
export DEVELOPER_DIR

NAME    := OpenLW
APP     := build/OpenLW.app
EXE     := $(APP)/Contents/MacOS/$(NAME)
SRC     := $(wildcard Sources/*.swift)
SDK     := $(shell DEVELOPER_DIR=$(DEVELOPER_DIR) xcrun --sdk macosx --show-sdk-path)
SWIFTC  := xcrun --sdk macosx swiftc
# /usr/lib/swift est déjà en rpath par défaut ; Frameworks sert au runtime embarqué (10.13 à 10.14.3).
FLAGS   := -swift-version 5 -O -g -sdk "$(SDK)" -framework AppKit -Xlinker -rpath -Xlinker @executable_path/../Frameworks
SIGN_ID ?= -

# Le nom de l'app contient une espace : les règles portent sur un témoin, pas sur le chemin du bundle.
STAMP   := build/app.stamp

all: $(STAMP)

build/x86_64/$(NAME): $(SRC)
	@mkdir -p $(dir $@)
	$(SWIFTC) -target x86_64-apple-macos10.13 $(FLAGS) $(SRC) -o $@

build/arm64/$(NAME): $(SRC)
	@mkdir -p $(dir $@)
	$(SWIFTC) -target arm64-apple-macos11.0 $(FLAGS) $(SRC) -o $@

$(STAMP): build/x86_64/$(NAME) build/arm64/$(NAME) Info.plist
	@rm -rf "$(APP)"
	@mkdir -p "$(APP)/Contents/MacOS" "$(APP)/Contents/Frameworks"
	lipo -create -output "$(EXE)" build/x86_64/$(NAME) build/arm64/$(NAME)
	cp Info.plist "$(APP)/Contents/Info.plist"
	xcrun swift-stdlib-tool --copy --platform macosx --scan-executable "$(EXE)" \
	  --destination "$(APP)/Contents/Frameworks" --source-libraries "$$(dirname $$(xcrun -f swiftc))/../lib/swift-5.0/macosx"
	codesign --force --deep --sign "$(SIGN_ID)" --timestamp=none "$(APP)"
	@touch $@

check-min: $(STAMP)
	@for a in x86_64 arm64; do printf '%s : ' $$a; \
	  otool -arch $$a -l "$(EXE)" | awk '/LC_VERSION_MIN_MACOSX|LC_BUILD_VERSION/{f=1} f&&/version|minos/{print $$2; f=0; exit}'; done
	@ls "$(APP)/Contents/Frameworks"

snapshot: $(STAMP)
	"$(EXE)" --snapshot "$(CURDIR)/build/snapshot.png"

install: $(STAMP)
	rm -rf "/Applications/OpenLW.app"
	cp -R "$(APP)" /Applications/

clean:
	rm -rf build

.PHONY: all check-min snapshot install clean
