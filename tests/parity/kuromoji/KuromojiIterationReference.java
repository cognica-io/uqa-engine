//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

import java.io.StringReader;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.security.MessageDigest;
import java.util.Base64;
import java.util.HexFormat;
import org.apache.lucene.analysis.ja.JapaneseIterationMarkCharFilter;

/** Complete iteration text and offset identities with bounded diagnostic snapshots. */
public class KuromojiIterationReference {
  static String matrix() {
    var input = new StringBuilder();
    for (int value = 0; value < 65536; value++) {
      if (Character.isSurrogate((char) value)) continue;
      for (char mark : new char[] {'々', 'ゝ', 'ゞ', 'ヽ', 'ヾ'}) input.append((char) value).append(mark).append('。');
    }
    return input.toString();
  }
  static void integer(MessageDigest hash, int value) {
    hash.update((byte) (value >>> 24)); hash.update((byte) (value >>> 16));
    hash.update((byte) (value >>> 8)); hash.update((byte) value);
  }
  static void text(MessageDigest hash, String value) {
    integer(hash, value.length());
    for (int i = 0; i < value.length(); i++) {
      hash.update((byte) (value.charAt(i) >>> 8)); hash.update((byte) value.charAt(i));
    }
  }
  static String units(String value) {
    var output = new StringBuilder("[");
    for (int i = 0; i < value.length(); i++) {
      if (i != 0) output.append(',');
      output.append((int) value.charAt(i));
    }
    return output.append(']').toString();
  }
  static String snapshot(String[] fields) throws Exception {
    String input = fields[1].equals("matrix") ? matrix() : new String(Base64.getDecoder().decode(fields[2]), StandardCharsets.UTF_8);
    boolean kanji = Boolean.parseBoolean(fields[3]), kana = Boolean.parseBoolean(fields[4]);
    int chunk = Integer.parseInt(fields[5]);
    try (var filter = new JapaneseIterationMarkCharFilter(new StringReader(input), kanji, kana)) {
      var output = new StringBuilder();
      if (chunk == 0) {
        for (int unit; (unit = filter.read()) != -1;) output.append((char) unit);
      } else {
        char[] buffer = new char[chunk + 2];
        for (int count; (count = filter.read(buffer, 1, chunk)) != -1;) output.append(buffer, 1, count);
      }
      var hash = MessageDigest.getInstance("SHA-256");
      text(hash, input); text(hash, output.toString()); integer(hash, output.length() + 1);
      var offsets = new StringBuilder("[");
      for (int i = 0; i <= output.length(); i++) {
        int corrected = filter.correctOffset(i);
        integer(hash, corrected);
        if (output.length() <= 128) {
          if (i != 0) offsets.append(',');
          offsets.append(corrected);
        }
      }
      String result = "{\"id\":\"" + fields[0] + "\",\"input_unit_count\":" + input.length()
          + ",\"output_unit_count\":" + output.length() + ",\"sha256\":\"" + HexFormat.of().formatHex(hash.digest()) + "\"";
      if (output.length() <= 128) result += ",\"output_utf16\":" + units(output.toString()) + ",\"offsets\":" + offsets.append(']');
      return result + "}";
    }
  }
  public static void main(String[] args) throws Exception {
    System.out.println("{\"runtime\":{\"java_version\":\"" + System.getProperty("java.version")
        + "\",\"java_runtime_version\":\"" + System.getProperty("java.runtime.version")
        + "\",\"java_vendor\":\"" + System.getProperty("java.vendor") + "\"}}");
    for (String row : Files.readAllLines(Path.of(args[0]), StandardCharsets.UTF_8)) System.out.println(snapshot(row.split("\t", -1)));
  }
}
