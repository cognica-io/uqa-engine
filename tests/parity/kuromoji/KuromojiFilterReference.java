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

import java.io.ByteArrayInputStream;
import java.io.DataInputStream;
import java.util.HashSet;
import java.util.Set;
import org.apache.lucene.analysis.CharArraySet;
import org.apache.lucene.analysis.LowerCaseFilter;
import org.apache.lucene.analysis.StopFilter;
import org.apache.lucene.analysis.ja.*;
import org.apache.lucene.util.AttributeImpl;
import org.apache.lucene.util.AttributeReflector;

/** Complete filter/analyzer observations with synthetic optional Japanese attributes. */
public class KuromojiFilterReference {
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
      return "[" + String.join(",", list.stream().map(KuromojiFilterReference::json).toList()) + "]";
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


  static DataInputStream data(String value) {
    return new DataInputStream(new ByteArrayInputStream(Base64.getDecoder().decode(value)));
  }
  static String text(DataInputStream input) throws Exception {
    int count = input.readInt();
    if (count < 0) return null;
    char[] value = new char[count];
    for (int i = 0; i < count; i++) value[i] = input.readChar();
    return new String(value);
  }
  static List<String> strings(String value) throws Exception {
    var input = data(value);
    int count = input.readInt();
    List<String> result = new ArrayList<>();
    for (int i = 0; i < count; i++) result.add(text(input));
    return result;
  }
  static class Morphology extends AttributeImpl implements BaseFormAttribute, PartOfSpeechAttribute, ReadingAttribute, InflectionAttribute {
    String pos, base, reading, pronunciation, type, form;
    public String getPartOfSpeech() { return pos; }
    public String getBaseForm() { return base; }
    public String getReading() { return reading; }
    public String getPronunciation() { return pronunciation; }
    public String getInflectionType() { return type; }
    public String getInflectionForm() { return form; }
    public void setToken(org.apache.lucene.analysis.ja.Token token) { throw new UnsupportedOperationException("synthetic attributes"); }
    public void clear() { pos = base = reading = pronunciation = type = form = null; }
    public void copyTo(AttributeImpl target) {
      var out = (Morphology) target;
      out.pos = pos; out.base = base; out.reading = reading; out.pronunciation = pronunciation; out.type = type; out.form = form;
    }
    public void reflectWith(AttributeReflector reflector) {
      reflector.reflect(BaseFormAttribute.class, "baseForm", base);
      reflector.reflect(PartOfSpeechAttribute.class, "partOfSpeech", pos);
      reflector.reflect(ReadingAttribute.class, "reading", reading);
      reflector.reflect(ReadingAttribute.class, "pronunciation", pronunciation);
      reflector.reflect(InflectionAttribute.class, "inflectionType", type);
      reflector.reflect(InflectionAttribute.class, "inflectionForm", form);
    }
  }
  static class Materialized extends TokenStream {
    final DataInputStream input;
    int remaining;
    final int finalOffset, finalIncrement;
    final Morphology morphology = new Morphology();
    final CharTermAttribute term = addAttribute(CharTermAttribute.class);
    final OffsetAttribute offset = addAttribute(OffsetAttribute.class);
    final PositionIncrementAttribute increment = addAttribute(PositionIncrementAttribute.class);
    final PositionLengthAttribute length = addAttribute(PositionLengthAttribute.class);
    final KeywordAttribute keyword = addAttribute(KeywordAttribute.class);
    Materialized(String[] fields) throws Exception {
      input = data(fields[9]); remaining = input.readInt();
      finalOffset = Integer.parseInt(fields[10]); finalIncrement = Integer.parseInt(fields[11]);
      addAttributeImpl(morphology);
    }
    public boolean incrementToken() throws java.io.IOException {
      if (remaining == 0) return false;
      clearAttributes();
      try {
        term.append(text(input));
        offset.setOffset(input.readInt(), input.readInt());
        increment.setPositionIncrement(input.readInt()); length.setPositionLength(input.readInt()); keyword.setKeyword(input.readBoolean());
        morphology.pos = text(input); morphology.base = text(input); morphology.reading = text(input); morphology.pronunciation = text(input); morphology.type = text(input); morphology.form = text(input);
      } catch (Exception error) { throw new java.io.IOException(error); }
      remaining--;
      return true;
    }
    public void end() { offset.setOffset(finalOffset, finalOffset); increment.setPositionIncrement(finalIncrement); }
  }
  static TokenStream chain(TokenStream input, String config) throws Exception {
    if (config.isEmpty()) return input;
    for (String item : config.split(",")) {
      String[] part = item.split(":", -1);
      input = switch (part[0]) {
        case "base" -> new JapaneseBaseFormFilter(input);
        case "pos" -> new JapanesePartOfSpeechStopFilter(input, part[1].equals("-") ? JapaneseAnalyzer.getDefaultStopTags() : new HashSet<>(strings(part[1])));
        case "stop" -> new StopFilter(input, part[2].equals("-") ? new CharArraySet(JapaneseAnalyzer.getDefaultStopSet(), Boolean.parseBoolean(part[1])) : new CharArraySet(strings(part[2]), Boolean.parseBoolean(part[1])));
        case "stem" -> new JapaneseKatakanaStemFilter(input, Integer.parseInt(part[1]));
        case "lower" -> new LowerCaseFilter(input);
        case "hiragana_uppercase" -> new JapaneseHiraganaUppercaseFilter(input);
        case "katakana_uppercase" -> new JapaneseKatakanaUppercaseFilter(input);
        case "reading" -> new JapaneseReadingFormFilter(input, Boolean.parseBoolean(part[1]));
        case "number" -> new JapaneseNumberFilter(input);
        case "completion" -> new JapaneseCompletionFilter(input, JapaneseCompletionFilter.Mode.valueOf(part[1]));
        default -> throw new IllegalArgumentException("unknown filter");
      };
    }
    return input;
  }
  static Map<String, Object> analyze(TokenStream stream) throws Exception {
    var term = stream.addAttribute(CharTermAttribute.class);
    var offsets = stream.addAttribute(OffsetAttribute.class);
    var increment = stream.addAttribute(PositionIncrementAttribute.class);
    var length = stream.addAttribute(PositionLengthAttribute.class);
    var keyword = stream.addAttribute(KeywordAttribute.class);
    var pos = stream.addAttribute(PartOfSpeechAttribute.class);
    var reading = stream.addAttribute(ReadingAttribute.class);
    var base = stream.addAttribute(BaseFormAttribute.class);
    var inflection = stream.addAttribute(InflectionAttribute.class);
    List<Object> tokens = new ArrayList<>();
    stream.reset();
    while (stream.incrementToken()) {
      tokens.add(object("term_utf16", units(term.toString()), "start_utf16", offsets.startOffset(), "end_utf16", offsets.endOffset(),
          "position_increment", increment.getPositionIncrement(), "position_length", length.getPositionLength(), "keyword", keyword.isKeyword(),
          "part_of_speech_utf16", nullableUnits(pos.getPartOfSpeech()), "base_form_utf16", nullableUnits(base.getBaseForm()),
          "reading_utf16", nullableUnits(reading.getReading()), "pronunciation_utf16", nullableUnits(reading.getPronunciation()),
          "inflection_type_utf16", nullableUnits(inflection.getInflectionType()), "inflection_form_utf16", nullableUnits(inflection.getInflectionForm())));
    }
    stream.end();
    return object("tokens", tokens, "final_offset_utf16", offsets.endOffset(), "final_position_increment", increment.getPositionIncrement());
  }
  static Map<String, Object> snapshot(String[] fields) throws Exception {
    var result = object("id", fields[0]);
    try {
      String input = rawInput(fields[2]);
      if (fields[1].equals("completion_romanize") || fields[1].equals("completion_units")) {
        List<Object> alternatives = new ArrayList<>();
        if (fields[1].equals("completion_romanize")) alternatives.addAll(romanize(input));
        else {
          for (int i = 0; i < input.length(); i++) {
            String value = String.valueOf(input.charAt(i));
            alternatives.add(romanize(value));
            alternatives.add(romanize("シ" + value + "カ"));
          }
        }
        String complete = json(alternatives);
        result.put("result_count", alternatives.size());
        result.put("sha256", digest(complete));
        if (alternatives.size() <= 12 && complete.length() <= 8192) result.put("results", alternatives);
        return result;
      }
      if (fields[1].equals("number_normalize") || fields[1].equals("number_units")) {
        try (var normalizer = new JapaneseNumberFilter(new Materialized(fields))) {
          if (fields[1].equals("number_normalize")) {
            var normalized = units(normalizer.normalizeNumber(input));
            result.put("normalized_unit_count", normalized.size());
            result.put("sha256", digest(json(normalized)));
            if (normalized.size() <= 512) result.put("normalized_utf16", normalized);
          } else {
            List<Object> normalized = new ArrayList<>();
            for (int i = 0; i < input.length(); i++) {
              String value = String.valueOf(input.charAt(i));
              normalized.add(units(normalizer.normalizeNumber(value)));
              normalized.add(units(normalizer.normalizeNumber("1" + value + "2")));
            }
            result.put("normalization_count", normalized.size());
            result.put("sha256", digest(json(normalized)));
          }
        }
        return result;
      }
      UserDictionary user = fields[7].equals("-") ? null : UserDictionary.open(new StringReader(new String(Base64.getDecoder().decode(fields[7]), StandardCharsets.UTF_8)));
      if (fields[1].equals("completion_analyzer") || fields[1].equals("completion_normalize")) {
        try (var analyzer = new JapaneseCompletionAnalyzer(user, JapaneseCompletionFilter.Mode.valueOf(fields[3]))) {
          if (fields[1].equals("completion_normalize")) { result.put("normalized_utf16", units(analyzer.normalize("body", input).utf8ToString())); return result; }
          try (var stream = analyzer.tokenStream("body", input)) { record(result, analyze(stream)); }
        }
        return result;
      }
      var mode = JapaneseTokenizer.Mode.valueOf(fields[3]);
      if (fields[1].equals("normalize") || fields[1].equals("analyzer")) {
        try (var analyzer = new JapaneseAnalyzer(user, mode, JapaneseAnalyzer.getDefaultStopSet(), JapaneseAnalyzer.getDefaultStopTags())) {
          if (fields[1].equals("normalize")) { result.put("normalized_utf16", units(analyzer.normalize("body", input).utf8ToString())); return result; }
          try (var stream = analyzer.tokenStream("body", input)) { record(result, analyze(stream)); }
        }
      } else {
        TokenStream source;
        if (fields[1].equals("tokens")) source = new Materialized(fields);
        else {
          var tokenizer = new JapaneseTokenizer(TokenStream.DEFAULT_TOKEN_ATTRIBUTE_FACTORY, user, Boolean.parseBoolean(fields[4]), Boolean.parseBoolean(fields[5]), mode);
          tokenizer.setNBestCost(Integer.parseInt(fields[6])); tokenizer.setReader(fields[1].equals("pipeline") ? new org.apache.lucene.analysis.cjk.CJKWidthCharFilter(new StringReader(input)) : new StringReader(input)); source = tokenizer;
        }
        try (var stream = chain(source, fields[8])) { record(result, analyze(stream)); }
      }
    } catch (Exception error) { result.put("error", error.getClass().getName()); }
    return result;
  }
  static List<Object> romanize(String input) {
    var romanizer = org.apache.lucene.analysis.ja.completion.KatakanaRomanizer.getInstance();
    List<Object> result = new ArrayList<>();
    for (var output : romanizer.romanize(new org.apache.lucene.util.CharsRef(input))) result.add(units(output.toString()));
    return result;
  }
  static void record(Map<String, Object> result, Map<String, Object> analysis) throws Exception {
    String complete = json(analysis);
    result.put("sha256", digest(complete));
    int count = ((List<?>) analysis.get("tokens")).size(); result.put("token_count", count);
    if (count <= 12 && complete.length() <= 8192) result.put("analysis", analysis);
  }
  static String digest(String value) throws Exception {
    return HexFormat.of().formatHex(MessageDigest.getInstance("SHA-256").digest(value.getBytes(StandardCharsets.UTF_8)));
  }
  public static void main(String[] args) throws Exception {
    System.out.println(json(object("runtime", object("java_version", System.getProperty("java.version"), "java_runtime_version", System.getProperty("java.runtime.version"), "java_vendor", System.getProperty("java.vendor")))));
    for (String row : Files.readAllLines(Path.of(args[0]), StandardCharsets.UTF_8)) System.out.println(json(snapshot(row.split("\t", -1))));
  }
}
