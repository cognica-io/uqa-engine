//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

import java.io.StringReader;
import java.security.MessageDigest;
import java.util.HexFormat;
import org.apache.lucene.analysis.cjk.CJKWidthCharFilter;

/** Stream complete text and corrected-offset identities without emitting the exhaustive corpus. */
public class CJKWidthReference {
  static final String[] EXAMPLES = {
    "", "UQA ＡＢＣ １２３！", "ｶﾞｯﾂﾎﾟｰｽﾞ", "ｳﾞｧｲｵﾘﾝ", "カﾞハﾟワﾞヰﾞヱﾞヲﾞヽﾞ",
    "ﾞﾟｶﾞﾞﾊﾟﾟ", "かﾞはﾟ", "Ａﾞ🙂ﾟ", "｡｢｣､･", "　①㍑ﬀÅＡ", "ガ\u3099ｶ\u3099",
    "🙂ｶﾞＡ🙂ﾊﾟZ", "ｳﾞﾞｳﾞﾟｳﾞ", "ｶﾞﾊﾟｶﾞ", "a\nｶﾞ\tﾊﾟ\rZ", "ｱﾞｶﾟﾝﾞ",
    "ヵﾞヶﾟヷﾞヴﾟ", "\u30a5ﾞ\u30a6ﾞ\u30fdﾞ\u30feﾞ", "\uff00\uff01\uff5e\uff5f\uff64\uff65\uff9f\uffa0"
  };

  record Result(String output, CJKWidthCharFilter filter) {}

  static Result filter(String input) throws Exception {
    var filter = new CJKWidthCharFilter(new StringReader(input));
    var output = new StringBuilder();
    char[] buffer = new char[1024];
    for (int count; (count = filter.read(buffer, 0, buffer.length)) != -1; ) {
      output.append(buffer, 0, count);
    }
    return new Result(output.toString(), filter);
  }

  static void integer(MessageDigest digest, int value) {
    digest.update((byte) (value >>> 24));
    digest.update((byte) (value >>> 16));
    digest.update((byte) (value >>> 8));
    digest.update((byte) value);
  }

  static void text(MessageDigest digest, String value) {
    integer(digest, value.length());
    for (int i = 0; i < value.length(); i++) {
      char unit = value.charAt(i);
      digest.update((byte) (unit >>> 8));
      digest.update((byte) unit);
    }
  }

  static void record(MessageDigest digest, String input) throws Exception {
    Result result = filter(input);
    text(digest, input);
    text(digest, result.output());
    integer(digest, result.output().length() + 1);
    for (int i = 0; i <= result.output().length(); i++) {
      integer(digest, result.filter().correctOffset(i));
    }
    result.filter().close();
  }

  static String scalars() throws Exception {
    var input = new StringBuilder();
    for (int scalar = 0; scalar <= Character.MAX_CODE_POINT; scalar++) {
      if (scalar < Character.MIN_SURROGATE || scalar > Character.MAX_SURROGATE) {
        input.appendCodePoint(scalar);
      }
    }
    var digest = MessageDigest.getInstance("SHA-256");
    record(digest, input.toString());
    return HexFormat.of().formatHex(digest.digest());
  }

  static String combinations() throws Exception {
    var digest = MessageDigest.getInstance("SHA-256");
    for (int[] range : new int[][] {{0x3000, 0x3100}, {0xff00, 0xffa0}}) {
      for (int scalar = range[0]; scalar <= range[1]; scalar++) {
        for (char first : new char[] {0xff9e, 0xff9f}) {
          for (char second : new char[] {0xff9e, 0xff9f}) {
            record(digest, new String(new char[] {(char) scalar, first, second}));
          }
        }
      }
    }
    return HexFormat.of().formatHex(digest.digest());
  }

  static String units(String text) {
    var output = new StringBuilder("[");
    for (int i = 0; i < text.length(); i++) {
      if (i != 0) output.append(',');
      output.append((int) text.charAt(i));
    }
    return output.append(']').toString();
  }

  public static void main(String[] args) throws Exception {
    System.out.print("{\"runtime\":{\"java_version\":\"" + System.getProperty("java.version")
        + "\",\"java_runtime_version\":\"" + System.getProperty("java.runtime.version")
        + "\",\"java_vendor\":\"" + System.getProperty("java.vendor") + "\"},"
        + "\"scalar_count\":1112064,\"scalar_sha256\":\"" + scalars()
        + "\",\"combination_count\":1672,\"combination_sha256\":\"" + combinations()
        + "\",\"examples\":[");
    for (int index = 0; index < EXAMPLES.length; index++) {
      if (index != 0) System.out.print(',');
      String input = EXAMPLES[index];
      Result result = filter(input);
      System.out.print("{\"input\":" + units(input) + ",\"output\":" + units(result.output())
          + ",\"offsets\":[");
      for (int i = 0; i <= result.output().length(); i++) {
        if (i != 0) System.out.print(',');
        System.out.print(result.filter().correctOffset(i));
      }
      result.filter().close();
      System.out.print("]}");
    }
    System.out.println("]}");
  }
}
