# Plugin HAL OpenLW : bundle universel (x86_64 10.13, arm64 11.0).
#   make            construit build/OpenLW.driver (signature ad hoc)
#   make test       banc d'essai : charge le bundle comme coreaudiod (CFPlugIn) et vérifie la chaîne
#   make check-min  affiche le plancher OS de chaque tranche
#   make install    copie dans /Library/Audio/Plug-Ins/HAL (sudo) et redémarre coreaudiod
#   make uninstall  retire le bundle (sudo) et redémarre coreaudiod

NAME     := OpenLW
BUNDLE   := build/$(NAME).driver
EXE      := $(BUNDLE)/Contents/MacOS/$(NAME)
LWSYS    := ../../daemon/lw-sys/csrc
SRC      := src/OpenLWPlugIn.c $(LWSYS)/lw_shm.c $(LWSYS)/macos/lw_sys_macos.c
CFLAGS   := -std=gnu11 -O2 -g -Wall -Wextra -Werror -fblocks -fvisibility=hidden -I$(LWSYS)
FRAMEWKS := -framework CoreAudio -framework CoreFoundation
HAL_DIR  := /Library/Audio/Plug-Ins/HAL
SIGN_ID  ?= -

all: $(BUNDLE)

build/x86_64/$(NAME): $(SRC) $(LWSYS)/lw_shm.h $(LWSYS)/lw_sys.h
	@mkdir -p $(dir $@)
	clang -arch x86_64 -mmacosx-version-min=10.13 $(CFLAGS) -bundle $(SRC) $(FRAMEWKS) -o $@

build/arm64/$(NAME): $(SRC) $(LWSYS)/lw_shm.h $(LWSYS)/lw_sys.h
	@mkdir -p $(dir $@)
	clang -arch arm64 -mmacosx-version-min=11.0 $(CFLAGS) -bundle $(SRC) $(FRAMEWKS) -o $@

$(BUNDLE): build/x86_64/$(NAME) build/arm64/$(NAME) Info.plist
	@mkdir -p $(BUNDLE)/Contents/MacOS
	lipo -create -output $(EXE) build/x86_64/$(NAME) build/arm64/$(NAME)
	cp Info.plist $(BUNDLE)/Contents/Info.plist
	codesign --force --sign "$(SIGN_ID)" --timestamp=none $(BUNDLE)

build/harness: test/harness.c $(LWSYS)/lw_shm.c $(LWSYS)/macos/lw_sys_macos.c $(BUNDLE)
	clang -std=gnu11 -O1 -g -Wall -Wextra -Werror -fblocks -I$(LWSYS) test/harness.c $(LWSYS)/lw_shm.c $(LWSYS)/macos/lw_sys_macos.c \
	  $(FRAMEWKS) -o $@

test: build/harness
	./build/harness $(BUNDLE)

check-min: $(BUNDLE)
	@otool -l build/x86_64/$(NAME) | grep -A3 LC_VERSION_MIN_MACOSX | grep version
	@xcrun vtool -show-build build/arm64/$(NAME) | grep minos
	@lipo -info $(EXE)

install: $(BUNDLE)
	sudo rm -rf $(HAL_DIR)/$(NAME).driver
	sudo cp -R $(BUNDLE) $(HAL_DIR)/
	sudo chown -R root:wheel $(HAL_DIR)/$(NAME).driver
	sudo killall coreaudiod

uninstall:
	sudo rm -rf $(HAL_DIR)/$(NAME).driver
	sudo killall coreaudiod

clean:
	rm -rf build

.PHONY: all test check-min install uninstall clean
