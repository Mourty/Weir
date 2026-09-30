# COPR builds Weir's Fedora and Nobara packages from this file, with its
# "make srpm" method. In a fresh clone, as root in a Fedora build root with
# network access, it runs
#
#   make -f .copr/Makefile srpm outdir=DIR spec=SPEC
#
# and then builds the package, for each Fedora release, from the source
# RPM left in DIR. Like `make rpm`, the source is the checked-out commit as
# `git archive` packs it. The release number stays 1: COPR builds each
# version once, from its release's tag.
#
# Anything else is refused: a commit that is not tagged with the version in
# the spec (v1.2.3 for 1.2.3) would be published as that release, with code
# that is not in it. COPR builds whatever its webhook asks for, and a webhook
# sending pushes asks for every commit on every branch.

top := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))..)
version := $(shell sed -n 's/^Version:[[:space:]]*//p' $(top)/packaging/weir.spec)
sources := /tmp/weir-srpm

.PHONY: srpm

srpm:
	dnf -y install git-core rpm-build
	@git -C $(top) tag --points-at HEAD | grep -qx 'v$(version)' || { \
		echo "Not building: COPR builds releases only, and this commit is not tagged v$(version)."; \
		exit 1; }
	mkdir -p $(sources) $(outdir)
	git -C $(top) archive --format=tar.gz --prefix=weir-$(version)/ \
		-o $(sources)/weir-$(version).tar.gz HEAD
	rpmbuild -bs --define "_sourcedir $(sources)" \
		--define "_srcrpmdir $(outdir)" $(top)/packaging/weir.spec
