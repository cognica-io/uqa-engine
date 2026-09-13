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
import org.apache.lucene.analysis.ko.KoreanTokenizer;
import org.apache.lucene.analysis.ko.dict.UserDictionary;
import org.apache.lucene.analysis.ko.tokenattributes.PartOfSpeechAttribute;
import org.apache.lucene.analysis.ko.tokenattributes.ReadingAttribute;
import org.apache.lucene.analysis.tokenattributes.CharTermAttribute;
import org.apache.lucene.analysis.tokenattributes.OffsetAttribute;
import org.apache.lucene.analysis.tokenattributes.PositionIncrementAttribute;
import org.apache.lucene.analysis.tokenattributes.PositionLengthAttribute;

import org.apache.lucene.analysis.Analyzer;
import org.apache.lucene.analysis.CharacterUtils;
import org.apache.lucene.analysis.LowerCaseFilter;
import org.apache.lucene.analysis.ko.KoreanAnalyzer;
import org.apache.lucene.analysis.ko.KoreanPartOfSpeechStopFilter;
import org.apache.lucene.analysis.ko.KoreanReadingFormFilter;
import org.apache.lucene.analysis.ko.POS;
import java.util.EnumSet;
import java.util.Set;

/** Independent filters, full KoreanAnalyzer, and normalization use the real pinned Lucene classes. */
public class NoriAnalysisReference {
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
      return "[" + String.join(",", list.stream().map(NoriAnalysisReference::json).toList()) + "]";
    }
    StringBuilder text = new StringBuilder("\"");
    for (char unit : value.toString().toCharArray()) {
      if (unit == '"' || unit == '\\') text.append('\\').append(unit);
      else if (unit < 32 || unit == 0x85 || unit == 0x2028 || unit == 0x2029 || Character.isSurrogate(unit)) text.append(String.format("\\u%04x", (int) unit));
      else text.append(unit);
    }
    return text.append('"').toString();
  }



  static Set<POS.Tag> tags(String value) {
    if (value.equals("*")) return KoreanPartOfSpeechStopFilter.DEFAULT_STOP_TAGS;
    Set<POS.Tag> tags = EnumSet.noneOf(POS.Tag.class);
    if (!value.isEmpty()) for (String name : value.split(",")) tags.add(POS.Tag.valueOf(name));
    return tags;
  }

  static Analyzer analyzer(String[] fields, UserDictionary user) {
    var mode = KoreanTokenizer.DecompoundMode.valueOf(fields[1]);
    boolean unigrams = Boolean.parseBoolean(fields[2]);
    if (fields[6].equals("analyzer") || fields[6].equals("normalize")) {
      return new KoreanAnalyzer(user, mode, KoreanPartOfSpeechStopFilter.DEFAULT_STOP_TAGS, unigrams);
    }
    return new Analyzer() {
      @Override
      protected TokenStreamComponents createComponents(String field) {
        var tokenizer = new KoreanTokenizer(TokenStream.DEFAULT_TOKEN_ATTRIBUTE_FACTORY, user,
            mode, unigrams, Boolean.parseBoolean(fields[3]));
        TokenStream stream = tokenizer;
        if (!fields[7].isEmpty()) for (String filter : fields[7].split(";")) {
          if (filter.startsWith("pos:")) stream = new KoreanPartOfSpeechStopFilter(stream, tags(filter.substring(4)));
          else if (filter.equals("reading")) stream = new KoreanReadingFormFilter(stream);
          else if (filter.equals("lowercase")) stream = new LowerCaseFilter(stream);
          else throw new IllegalArgumentException("unknown filter: " + filter);
        }
        return new TokenStreamComponents(tokenizer, stream);
      }
    };
  }

  static Map<String, Object> unicode(String id) throws Exception {
    var text = new StringBuilder();
    for (int point = 0; point <= Character.MAX_CODE_POINT; point++) {
      if (point < 0xd800 || point > 0xdfff) text.appendCodePoint(point);
    }
    for (int unit = 0xd800; unit <= 0xdfff; unit++) text.append((char) unit).append('!');
    char[] units = text.toString().toCharArray();
    CharacterUtils.toLowerCase(units, 0, units.length);
    var digest = MessageDigest.getInstance("SHA-256");
    for (char unit : units) {
      digest.update((byte) (unit >>> 8));
      digest.update((byte) unit);
    }
    return object("id", id, "unit_count", units.length, "utf16be_sha256", HexFormat.of().formatHex(digest.digest()));
  }

  static Map<String, Object> snapshot(String[] fields) throws Exception {
    if (fields[6].equals("unicode_lowercase")) return unicode(fields[0]);
    String input = new String(Base64.getDecoder().decode(fields[4]), StandardCharsets.UTF_8);
    String rules = fields[5].equals("-") ? null : new String(Base64.getDecoder().decode(fields[5]), StandardCharsets.UTF_8);
    var result = object("id", fields[0]);
    try {
      UserDictionary user = rules == null ? null : UserDictionary.open(new StringReader(rules));
      try (Analyzer analyzer = analyzer(fields, user)) {
        if (fields[6].equals("normalize")) {
          result.put("normalized_utf16", units(analyzer.normalize("body", input).utf8ToString()));
          return result;
        }
        try (TokenStream stream = analyzer.tokenStream("body", input)) {
          var term = stream.addAttribute(CharTermAttribute.class);
          var offsets = stream.addAttribute(OffsetAttribute.class);
          var increment = stream.addAttribute(PositionIncrementAttribute.class);
          var length = stream.addAttribute(PositionLengthAttribute.class);
          var pos = stream.addAttribute(PartOfSpeechAttribute.class);
          var reading = stream.addAttribute(ReadingAttribute.class);
          List<Object> tokens = new ArrayList<>();
          stream.reset();
          while (stream.incrementToken()) {
            List<Object> parts = null;
            if (pos.getMorphemes() != null) {
              parts = new ArrayList<>();
              for (var part : pos.getMorphemes()) parts.add(object("surface_utf16", units(part.surfaceForm()), "pos", part.posTag()));
            }
            tokens.add(object("term_utf16", units(term.toString()), "start_utf16", offsets.startOffset(), "end_utf16", offsets.endOffset(),
                "position_increment", increment.getPositionIncrement(), "position_length", length.getPositionLength(), "pos_type", pos.getPOSType(),
                "left_pos", pos.getLeftPOS(), "right_pos", pos.getRightPOS(), "reading_utf16", reading.getReading() == null ? null : units(reading.getReading()), "morphemes", parts));
          }
          stream.end();
          var analysis = object("tokens", tokens, "final_offset_utf16", offsets.endOffset(), "final_position_increment", increment.getPositionIncrement());
          result.put("sha256", HexFormat.of().formatHex(MessageDigest.getInstance("SHA-256").digest(json(analysis).getBytes(StandardCharsets.UTF_8))));
          result.put("token_count", tokens.size());
          if (tokens.size() <= 32) result.put("analysis", analysis);
        }
      }
    } catch (IllegalArgumentException error) { result.put("error", error.getClass().getName()); }
    return result;
  }

  public static void main(String[] args) throws Exception {
    System.out.println(json(object("runtime", object("java_version", System.getProperty("java.version"),
        "java_runtime_version", System.getProperty("java.runtime.version"), "java_vendor", System.getProperty("java.vendor")))));
    for (String row : Files.readAllLines(Path.of(args[0]), StandardCharsets.UTF_8)) System.out.println(json(snapshot(row.split("\t", -1))));
  }
}
