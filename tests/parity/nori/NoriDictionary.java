//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

import java.nio.file.Path;
import org.apache.lucene.analysis.ko.dict.DictionaryBuilder;

/** Rebuild the pinned CSV source through Lucene's public dictionary builder. */
class NoriDictionary {
  public static void main(String[] args) throws Exception {
    if (args.length != 2) {
      throw new IllegalArgumentException("usage: NoriDictionary.java input-directory output-directory");
    }
    DictionaryBuilder.build(Path.of(args[0]), Path.of(args[1]), "utf-8", false);
    System.out.println(System.getProperty("java.version"));
    System.out.println(System.getProperty("java.runtime.version"));
    System.out.println(System.getProperty("java.vendor"));
  }
}
