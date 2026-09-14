// Unified Query Algebra
// Copyright (c) 2023-2026 Cognica, Inc.

import java.io.StringReader;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.security.MessageDigest;
import java.util.ArrayList;
import java.util.Base64;
import java.util.HexFormat;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import org.apache.lucene.analysis.TokenStream;
import org.apache.lucene.analysis.ja.JapaneseTokenizer;
import org.apache.lucene.analysis.ja.dict.UserDictionary;
import org.apache.lucene.analysis.ja.tokenattributes.BaseFormAttribute;
import org.apache.lucene.analysis.ja.tokenattributes.InflectionAttribute;
import org.apache.lucene.analysis.ja.tokenattributes.PartOfSpeechAttribute;
import org.apache.lucene.analysis.ja.tokenattributes.ReadingAttribute;
import org.apache.lucene.analysis.tokenattributes.CharTermAttribute;
import org.apache.lucene.analysis.tokenattributes.KeywordAttribute;
import org.apache.lucene.analysis.tokenattributes.OffsetAttribute;
import org.apache.lucene.analysis.tokenattributes.PositionIncrementAttribute;
import org.apache.lucene.analysis.tokenattributes.PositionLengthAttribute;

/** Complete public Japanese token attributes, with bounded snapshots and exhaustive stream hashes. */
public class KuromojiTokenizerReference {
  static Map<String, Object> object(Object... pairs) {
    Map<String, Object> value = new LinkedHashMap<>();
    for (int i = 0; i < pairs.length; i += 2) value.put((String) pairs[i], pairs[i + 1]);
    return value;
  }

  static List<Integer> units(String text) {
    List<Integer> value = new ArrayList<>();
    for (char unit : text.toCharArray()) value.add((int) unit);
    return value;
  }

  static String json(Object value) {
    if (value == null) return "null";
    if (value instanceof Number || value instanceof Boolean) return value.toString();
    if (value instanceof Map<?, ?> map) {
      List<String> fields = new ArrayList<>();
      var sorted = new java.util.TreeMap<String, Object>();
      map.forEach((key, item) -> sorted.put(key.toString(), item));
      sorted.forEach((key, item) -> fields.add(json(key) + ":" + json(item)));
      return "{" + String.join(",", fields) + "}";
    }
    if (value instanceof List<?> list) {
      return "[" + String.join(",", list.stream().map(KuromojiTokenizerReference::json).toList()) + "]";
    }
    StringBuilder text = new StringBuilder("\"");
    for (char unit : value.toString().toCharArray()) {
      if (unit == '"' || unit == '\\') text.append('\\').append(unit);
      else if (unit < 32 || unit == 0x85 || unit == 0x2028 || unit == 0x2029 || Character.isSurrogate(unit)) text.append(String.format("\\u%04x", (int) unit));
      else text.append(unit);
    }
    return text.append('"').toString();
  }


  static Object nullableUnits(String text) {
    return text == null ? null : units(text);
  }

  static String rawInput(String field) {
    byte[] bytes = Base64.getDecoder().decode(field);
    if (bytes.length % 2 != 0) throw new IllegalArgumentException("unaligned UTF-16 input");
    char[] units = new char[bytes.length / 2];
    for (int i = 0; i < units.length; i++) units[i] = (char) (((bytes[2 * i] & 255) << 8) | (bytes[2 * i + 1] & 255));
    return new String(units);
  }

  static Map<String, Object> snapshot(String[] fields) throws Exception {
    String input = rawInput(fields[4]);
    String rules = fields[5].equals("-") ? null : new String(Base64.getDecoder().decode(fields[5]), StandardCharsets.UTF_8);
    var result = object("id", fields[0]);
    String stage = "compile";
    try {
      UserDictionary user = rules == null ? null : UserDictionary.open(new StringReader(rules));
      try (var tokenizer = new JapaneseTokenizer(TokenStream.DEFAULT_TOKEN_ATTRIBUTE_FACTORY, user,
          Boolean.parseBoolean(fields[2]), Boolean.parseBoolean(fields[3]), JapaneseTokenizer.Mode.valueOf(fields[1]))) {
        stage = "configure";
        int cost = Integer.parseInt(fields[6]);
        if (!fields[7].equals("-")) {
          int derived = tokenizer.calcNBestCost(new String(Base64.getDecoder().decode(fields[7]), StandardCharsets.UTF_8));
          result.put("example_cost", derived);
          cost = Math.max(cost, derived);
        }
        tokenizer.setNBestCost(cost);
        tokenizer.setReader(new StringReader(input));
        var term = tokenizer.addAttribute(CharTermAttribute.class);
        var offsets = tokenizer.addAttribute(OffsetAttribute.class);
        var increment = tokenizer.addAttribute(PositionIncrementAttribute.class);
        var length = tokenizer.addAttribute(PositionLengthAttribute.class);
        var keyword = tokenizer.addAttribute(KeywordAttribute.class);
        var pos = tokenizer.addAttribute(PartOfSpeechAttribute.class);
        var reading = tokenizer.addAttribute(ReadingAttribute.class);
        var base = tokenizer.addAttribute(BaseFormAttribute.class);
        var inflection = tokenizer.addAttribute(InflectionAttribute.class);
        List<Object> tokens = new ArrayList<>();
        stage = "tokenize";
        tokenizer.reset();
        while (tokenizer.incrementToken()) {
          stage = "attributes";
          tokens.add(object("term_utf16", units(term.toString()), "start_utf16", offsets.startOffset(), "end_utf16", offsets.endOffset(),
              "position_increment", increment.getPositionIncrement(), "position_length", length.getPositionLength(), "keyword", keyword.isKeyword(),
              "part_of_speech_utf16", nullableUnits(pos.getPartOfSpeech()), "base_form_utf16", nullableUnits(base.getBaseForm()),
              "reading_utf16", nullableUnits(reading.getReading()), "pronunciation_utf16", nullableUnits(reading.getPronunciation()),
              "inflection_type_utf16", nullableUnits(inflection.getInflectionType()), "inflection_form_utf16", nullableUnits(inflection.getInflectionForm())));
          stage = "tokenize";
        }
        tokenizer.end();
        var analysis = object("tokens", tokens, "final_offset_utf16", offsets.endOffset(), "final_position_increment", increment.getPositionIncrement());
        result.put("sha256", HexFormat.of().formatHex(MessageDigest.getInstance("SHA-256").digest(json(analysis).getBytes(StandardCharsets.UTF_8))));
        result.put("token_count", tokens.size());
        if (tokens.size() <= 12) result.put("analysis", analysis);
      }
    } catch (Exception error) {
      result.put("error", error.getClass().getName());
      result.put("error_stage", stage);
    }
    return result;
  }

  public static void main(String[] args) throws Exception {
    System.out.println(json(object("runtime", object("java_version", System.getProperty("java.version"),
        "java_runtime_version", System.getProperty("java.runtime.version"), "java_vendor", System.getProperty("java.vendor")))));
    for (String row : Files.readAllLines(Path.of(args[0]), StandardCharsets.UTF_8)) System.out.println(json(snapshot(row.split("\t", -1))));
  }
}
