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

import org.apache.lucene.analysis.ko.KoreanNumberFilter;
import org.apache.lucene.analysis.ko.Token;
import org.apache.lucene.analysis.ko.dict.KoMorphData;
import org.apache.lucene.analysis.morph.TokenType;
import org.apache.lucene.analysis.tokenattributes.KeywordAttribute;

/** Full number attributes, direct decimal normalization, and controlled graph/EOF sources. */
public class NoriNumberReference {
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
      return "[" + String.join(",", list.stream().map(NoriNumberReference::json).toList()) + "]";
    }
    StringBuilder text = new StringBuilder("\"");
    for (char unit : value.toString().toCharArray()) {
      if (unit == '"' || unit == '\\') text.append('\\').append(unit);
      else if (unit < 32 || unit == 0x85 || unit == 0x2028 || unit == 0x2029 || Character.isSurrogate(unit)) text.append(String.format("\\u%04x", (int) unit));
      else text.append(unit);
    }
    return text.append('"').toString();
  }




  static String text(String hex) {
    char[] result = new char[hex.length() / 4];
    for (int i = 0; i < result.length; i++) result[i] = (char) Integer.parseInt(hex.substring(i * 4, i * 4 + 4), 16);
    return new String(result);
  }

  static class MetadataToken extends Token {
    final String[] fields;
    MetadataToken(String[] fields) {
      super(text(fields[0]).toCharArray(), 0, fields[0].length() / 4,
          Integer.parseInt(fields[1]), Integer.parseInt(fields[2]), TokenType.KNOWN);
      this.fields = fields;
    }
    public POS.Type getPOSType() { return POS.Type.valueOf(fields[6]); }
    public POS.Tag getLeftPOS() { return POS.Tag.valueOf(fields[7]); }
    public POS.Tag getRightPOS() { return POS.Tag.valueOf(fields[8]); }
    public String getReading() { return fields[9].equals("-") ? null : text(fields[9]); }
    public KoMorphData.Morpheme[] getMorphemes() {
      if (fields[10].equals("-")) return null;
      if (fields[10].isEmpty()) return new KoMorphData.Morpheme[0];
      List<KoMorphData.Morpheme> parts = new ArrayList<>();
      for (String part : fields[10].split(";")) {
        String[] values = part.split(":", -1);
        parts.add(new KoMorphData.Morpheme(POS.Tag.valueOf(values[0]), text(values[1])));
      }
      return parts.toArray(new KoMorphData.Morpheme[0]);
    }
  }

  static final class SyntheticStream extends TokenStream {
    final String[] tokens;
    final int finalOffset;
    final int finalIncrement;
    int index;
    final CharTermAttribute term = addAttribute(CharTermAttribute.class);
    final OffsetAttribute offsets = addAttribute(OffsetAttribute.class);
    final PositionIncrementAttribute increment = addAttribute(PositionIncrementAttribute.class);
    final PositionLengthAttribute length = addAttribute(PositionLengthAttribute.class);
    final KeywordAttribute keyword = addAttribute(KeywordAttribute.class);
    final PartOfSpeechAttribute pos = addAttribute(PartOfSpeechAttribute.class);
    final ReadingAttribute reading = addAttribute(ReadingAttribute.class);
    SyntheticStream(String encoded, int offset, int increment) {
      String source = new String(Base64.getDecoder().decode(encoded), StandardCharsets.US_ASCII);
      tokens = source.isEmpty() ? new String[0] : source.split("\n");
      finalOffset = offset;
      finalIncrement = increment;
    }
    @Override public void reset() throws java.io.IOException { super.reset(); index = 0; }
    @Override public boolean incrementToken() {
      if (index == tokens.length) return false;
      clearAttributes();
      String[] fields = tokens[index++].split("\t", -1);
      var metadata = new MetadataToken(fields);
      term.append(text(fields[0]));
      offsets.setOffset(metadata.getStartOffset(), metadata.getEndOffset());
      increment.setPositionIncrement(Integer.parseInt(fields[3]));
      length.setPositionLength(Integer.parseInt(fields[4]));
      keyword.setKeyword(Boolean.parseBoolean(fields[5]));
      pos.setToken(metadata);
      reading.setToken(metadata);
      return true;
    }
    @Override public void end() throws java.io.IOException {
      super.end();
      offsets.setOffset(finalOffset, finalOffset);
      increment.setPositionIncrement(finalIncrement);
    }
  }

  static TokenStream filters(TokenStream stream, String specification) {
    if (!specification.isEmpty()) for (String filter : specification.split(";")) {
      if (filter.equals("number")) stream = new KoreanNumberFilter(stream);
      else if (filter.equals("reading")) stream = new KoreanReadingFormFilter(stream);
      else if (filter.equals("lowercase")) stream = new LowerCaseFilter(stream);
      else if (filter.startsWith("pos:")) {
        Set<POS.Tag> tags = EnumSet.noneOf(POS.Tag.class);
        String source = filter.substring(4);
        if (source.equals("*")) tags = KoreanPartOfSpeechStopFilter.DEFAULT_STOP_TAGS;
        else if (!source.isEmpty()) for (String name : source.split(",")) tags.add(POS.Tag.valueOf(name));
        stream = new KoreanPartOfSpeechStopFilter(stream, tags);
      } else throw new IllegalArgumentException("unknown filter: " + filter);
    }
    return stream;
  }

  static Map<String, Object> normalized(String id, String input) throws Exception {
    String normalized;
    try (var filter = new KoreanNumberFilter(new SyntheticStream("", 0, 0))) {
      normalized = filter.normalizeNumber(input);
    }
    var digest = MessageDigest.getInstance("SHA-256");
    for (char unit : normalized.toCharArray()) {
      digest.update((byte) (unit >>> 8));
      digest.update((byte) unit);
    }
    var result = object("id", id, "unit_count", normalized.length(), "utf16be_sha256", HexFormat.of().formatHex(digest.digest()));
    if (normalized.length() <= 256) result.put("normalized_utf16", units(normalized));
    return result;
  }

  static Map<String, Object> snapshot(String[] fields) throws Exception {
    String input = text(fields[2]);
    if (fields[1].equals("normalize")) return normalized(fields[0], input);
    var result = object("id", fields[0]);
    try {
      TokenStream source;
      if (fields[1].equals("synthetic")) {
        source = new SyntheticStream(fields[8], Integer.parseInt(fields[9]), Integer.parseInt(fields[10]));
      } else {
        String rules = fields[6].equals("-") ? null : new String(Base64.getDecoder().decode(fields[6]), StandardCharsets.UTF_8);
        UserDictionary user = rules == null ? null : UserDictionary.open(new StringReader(rules));
        var tokenizer = new KoreanTokenizer(TokenStream.DEFAULT_TOKEN_ATTRIBUTE_FACTORY, user,
            KoreanTokenizer.DecompoundMode.valueOf(fields[3]), Boolean.parseBoolean(fields[4]), Boolean.parseBoolean(fields[5]));
        tokenizer.setReader(new StringReader(input));
        source = tokenizer;
      }
      try (TokenStream stream = filters(source, fields[7])) {
        var term = stream.addAttribute(CharTermAttribute.class);
        var offsets = stream.addAttribute(OffsetAttribute.class);
        var increment = stream.addAttribute(PositionIncrementAttribute.class);
        var length = stream.addAttribute(PositionLengthAttribute.class);
        var keyword = stream.addAttribute(KeywordAttribute.class);
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
              "position_increment", increment.getPositionIncrement(), "position_length", length.getPositionLength(), "keyword", keyword.isKeyword(),
              "pos_type", pos.getPOSType(), "left_pos", pos.getLeftPOS(), "right_pos", pos.getRightPOS(),
              "reading_utf16", reading.getReading() == null ? null : units(reading.getReading()), "morphemes", parts));
        }
        stream.end();
        var analysis = object("tokens", tokens, "final_offset_utf16", offsets.endOffset(), "final_position_increment", increment.getPositionIncrement());
        result.put("sha256", HexFormat.of().formatHex(MessageDigest.getInstance("SHA-256").digest(json(analysis).getBytes(StandardCharsets.UTF_8))));
        result.put("token_count", tokens.size());
        if (tokens.size() <= 32) result.put("analysis", analysis);
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
