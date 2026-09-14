//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

import java.nio.file.Path;
import org.apache.lucene.analysis.ja.dict.DictionaryBuilder;

/** Rebuild the patched IPADIC inputs with Lucene's exact format, encoding and normalization policy. */
class KuromojiDictionary {
  public static void main(String[] args) throws Exception {
    if (args.length != 2) {
      throw new IllegalArgumentException("usage: KuromojiDictionary.java input-directory output-directory");
    }
    DictionaryBuilder.build(DictionaryBuilder.DictionaryFormat.IPADIC,
        Path.of(args[0]), Path.of(args[1]), "euc-jp", false);
    System.out.println(System.getProperty("java.version"));
    System.out.println(System.getProperty("java.runtime.version"));
    System.out.println(System.getProperty("java.vendor"));
  }
}
