#!/usr/bin/env bash
# Regenerates the pinned MATSim reference output used by the Rust differential fixtures.
#
# The reference is MATSim tag 2026.0, commit c7a75ebeddc3ceb62959af046190064bf23770df. It is not
# published to Maven Central, so the launcher checks the commit out, verifies it, and builds it
# into a local Maven repository before running the harness against the shared fixtures.
#
# Requirements: git, bash, Maven 3.9+, and a JDK 25 (MATSim 2026.0 sets maven.compiler.release=25).
# Set JAVA_HOME to a JDK 25. SetReference_CACHE_DIR to relocate the checkout and Maven repository.
#
# Usage: java_reference/run_reference.sh [fixture-name ...]
#        with no arguments every fixture below matsim_rust/tests/resources/pt_reference is recorded.
# See docs/pt_java_reference.md for the fixture contract and comparison rules.
set -euo pipefail

REFERENCE_COMMIT=c7a75ebeddc3ceb62959af046190064bf23770df
REFERENCE_TAG=2026.0
REFERENCE_REPOSITORY=https://github.com/matsim-org/matsim

HARNESS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(dirname "$HARNESS_DIR")"
FIXTURE_DIR="$REPO_DIR/matsim_rust/tests/resources/pt_reference"
OUTPUT_DIR="$FIXTURE_DIR/java"
CACHE_DIR="${REFERENCE_CACHE_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}/matsim-rust/java-reference}"
MATSIM_DIR="$CACHE_DIR/matsim"
MAVEN_REPOSITORY="$CACHE_DIR/m2"

: "${JAVA_HOME:?set JAVA_HOME to a JDK 25; MATSim 2026.0 compiles with maven.compiler.release=25}"
JAVA_MAJOR="$("$JAVA_HOME/bin/java" -version 2>&1 | sed -nE 's/.*version "([0-9]+).*/\1/p')"
if [ "$JAVA_MAJOR" -lt 25 ]; then
  echo "MATSim 2026.0 needs a JDK 25, found $JAVA_MAJOR at $JAVA_HOME" >&2
  exit 1
fi
export JAVA_HOME
export PATH="$JAVA_HOME/bin:$PATH"

mkdir -p "$CACHE_DIR" "$OUTPUT_DIR"

# 1. Check out the pinned reference, once.
if [ ! -d "$MATSIM_DIR/.git" ]; then
  echo "checking out $REFERENCE_REPOSITORY $REFERENCE_TAG into $MATSIM_DIR"
  git clone --depth 1 --branch "$REFERENCE_TAG" "$REFERENCE_REPOSITORY" "$MATSIM_DIR"
fi
ACTUAL_COMMIT="$(git -C "$MATSIM_DIR" rev-parse HEAD)"
if [ "$ACTUAL_COMMIT" != "$REFERENCE_COMMIT" ]; then
  echo "reference checkout is $ACTUAL_COMMIT, expected $REFERENCE_COMMIT" >&2
  exit 1
fi

# 2. Build the reference and SBB transit extension into a local Maven repository.
if [ ! -f "$MAVEN_REPOSITORY/org/matsim/matsim/$REFERENCE_TAG/matsim-$REFERENCE_TAG.jar" ]; then
  echo "building MATSim $REFERENCE_TAG"
  mvn -B -f "$MATSIM_DIR/pom.xml" -pl matsim -am \
    -Dmaven.test.skip=true -Dcheckstyle.skip=true -Denforcer.skip=true \
    -Dmaven.repo.local="$MAVEN_REPOSITORY" install
fi
if [ ! -f "$MAVEN_REPOSITORY/org/matsim/contrib/sbb-extensions/$REFERENCE_TAG/sbb-extensions-$REFERENCE_TAG.jar" ]; then
  echo "building MATSim SBB transit extension $REFERENCE_TAG"
  mvn -B -f "$MATSIM_DIR/pom.xml" -pl contribs/sbb-extensions -am \
    -Dmaven.test.skip=true -Dcheckstyle.skip=true -Denforcer.skip=true \
    -Dmaven.repo.local="$MAVEN_REPOSITORY" install
fi

# 3. Build the harness against it and resolve its classpath once.
mvn -B -q -f "$HARNESS_DIR/pom.xml" -Dmaven.repo.local="$MAVEN_REPOSITORY" package
mvn -B -q -f "$HARNESS_DIR/pom.xml" -Dmaven.repo.local="$MAVEN_REPOSITORY" \
  dependency:build-classpath -Dmdep.outputFile="$CACHE_DIR/classpath.txt"
CLASSPATH="$HARNESS_DIR/target/classes:$(cat "$CACHE_DIR/classpath.txt")"

# 4. Record every requested fixture. Each fixture directory holds a MATSim `config.xml`, an optional
#    `requests.json`, and is recorded as java/<fixture>.json.
fixtures=("$@")
if [ ${#fixtures[@]} -eq 0 ]; then
  while IFS= read -r config; do
    fixtures+=("$(basename "$(dirname "$config")")")
  done < <(find "$FIXTURE_DIR" -mindepth 2 -maxdepth 2 -name config.xml | sort)
fi

for fixture in "${fixtures[@]}"; do
  config="$FIXTURE_DIR/$fixture/config.xml"
  [ -f "$config" ] || { echo "no $fixture fixture in $FIXTURE_DIR" >&2; exit 1; }
  requests=()
  [ -f "$FIXTURE_DIR/$fixture/requests.json" ] && requests=(--requests "$FIXTURE_DIR/$fixture/requests.json")
  echo "recording $fixture"
  (cd "$FIXTURE_DIR/$fixture" && "$JAVA_HOME/bin/java" -cp "$CLASSPATH" \
    org.matsimrust.reference.ReferenceMain \
    --config config.xml \
    --out "$OUTPUT_DIR/$fixture.json" \
    --reference-version "$REFERENCE_TAG" \
    --reference-commit "$REFERENCE_COMMIT" \
    "${requests[@]}")
done

echo "recorded ${#fixtures[@]} fixture(s) into $OUTPUT_DIR"
