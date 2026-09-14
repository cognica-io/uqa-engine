// Unified Query Algebra
// Copyright (c) 2023-2026 Cognica, Inc.

import java.io.StringReader;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import org.apache.lucene.analysis.Analyzer;
import org.apache.lucene.analysis.LowerCaseFilter;
import org.apache.lucene.analysis.TokenStream;
import org.apache.lucene.analysis.Tokenizer;
import org.apache.lucene.analysis.core.KeywordTokenizer;
import org.apache.lucene.analysis.ko.KoreanAnalyzer;
import org.apache.lucene.analysis.ko.KoreanNumberFilter;
import org.apache.lucene.analysis.ko.KoreanPartOfSpeechStopFilter;
import org.apache.lucene.analysis.ko.KoreanTokenizer;
import org.apache.lucene.analysis.ko.dict.UserDictionary;
import org.apache.lucene.analysis.ko.tokenattributes.PartOfSpeechAttribute;
import org.apache.lucene.analysis.ko.tokenattributes.ReadingAttribute;
import org.apache.lucene.analysis.tokenattributes.CharTermAttribute;
import org.apache.lucene.analysis.tokenattributes.OffsetAttribute;
import org.apache.lucene.analysis.tokenattributes.PositionIncrementAttribute;
import org.apache.lucene.analysis.tokenattributes.PositionLengthAttribute;

/** Small executable reference for design examples, not a UQA parity claim. */
public class NoriReference {
  record Case(
      String id, String input, String pipeline, String mode,
      boolean unknownUnigrams, boolean discardPunctuation, String userDictionary) {}

  static Map<String, Object> object(Object... pairs) {
    Map<String, Object> result = new LinkedHashMap<>();
    for (int i = 0; i < pairs.length; i += 2) result.put((String) pairs[i], pairs[i + 1]);
    return result;
  }

  static String json(Object value) {
    if (value == null) return "null";
    if (value instanceof String text) {
      StringBuilder result = new StringBuilder("\"");
      for (char c : text.toCharArray()) {
        switch (c) {
          case '"' -> result.append("\\\"");
          case '\\' -> result.append("\\\\");
          case '\n' -> result.append("\\n");
          case '\r' -> result.append("\\r");
          case '\t' -> result.append("\\t");
          default -> {
            if (c < 32 || Character.isSurrogate(c)) {
              result.append(String.format("\\u%04x", (int) c));
            } else result.append(c);
          }
        }
      }
      return result.append('"').toString();
    }
    if (value instanceof Map<?, ?> map) {
      List<String> entries = new ArrayList<>();
      map.forEach((key, item) -> entries.add(json(key) + ":" + json(item)));
      return "{" + String.join(",", entries) + "}";
    }
    if (value instanceof List<?> list) {
      return "[" + String.join(",", list.stream().map(NoriReference::json).toList()) + "]";
    }
    if (value instanceof Number || value instanceof Boolean) return value.toString();
    return json(value.toString());
  }

  static Analyzer analyzer(Case fixture) throws Exception {
    UserDictionary dictionary = fixture.userDictionary() == null ? null
        : UserDictionary.open(new StringReader(fixture.userDictionary()));
    if (fixture.pipeline().equals("analyzer")) {
      return new KoreanAnalyzer(dictionary, KoreanTokenizer.DecompoundMode.valueOf(fixture.mode()),
          KoreanPartOfSpeechStopFilter.DEFAULT_STOP_TAGS, fixture.unknownUnigrams());
    }
    return new Analyzer() {
      @Override
      protected TokenStreamComponents createComponents(String field) {
        Tokenizer tokenizer;
        TokenStream stream;
        if (fixture.pipeline().equals("lowercase")) {
          tokenizer = new KeywordTokenizer();
          stream = new LowerCaseFilter(tokenizer);
        } else {
          tokenizer = new KoreanTokenizer(TokenStream.DEFAULT_TOKEN_ATTRIBUTE_FACTORY,
              dictionary, KoreanTokenizer.DecompoundMode.valueOf(fixture.mode()),
              fixture.unknownUnigrams(), fixture.discardPunctuation());
          stream = tokenizer;
          if (fixture.pipeline().equals("number")) {
            stream = new KoreanNumberFilter(stream);
          }
        }
        return new TokenStreamComponents(tokenizer, stream);
      }
    };
  }

  static void emit(Case fixture) throws Exception {
    Map<String, Object> result = object(
        "id", fixture.id(), "input", fixture.input(), "pipeline", fixture.pipeline(),
        "decompound_mode", fixture.mode().toLowerCase(java.util.Locale.ROOT),
        "output_unknown_unigrams", fixture.unknownUnigrams(),
        "discard_punctuation", fixture.discardPunctuation(),
        "user_dictionary", fixture.userDictionary());
    try (Analyzer analyzer = analyzer(fixture);
         TokenStream stream = analyzer.tokenStream("body", fixture.input())) {
      var term = stream.addAttribute(CharTermAttribute.class);
      var offsets = stream.addAttribute(OffsetAttribute.class);
      var increment = stream.addAttribute(PositionIncrementAttribute.class);
      var length = stream.addAttribute(PositionLengthAttribute.class);
      var pos = stream.addAttribute(PartOfSpeechAttribute.class);
      var reading = stream.addAttribute(ReadingAttribute.class);
      List<Object> tokens = new ArrayList<>();
      stream.reset();
      int position = -1;
      while (stream.incrementToken()) {
        position += increment.getPositionIncrement();
        List<Object> morphemes = null;
        if (pos.getMorphemes() != null) {
          morphemes = new ArrayList<>();
          for (var morpheme : pos.getMorphemes()) {
            morphemes.add(object("surface", morpheme.surfaceForm(), "pos", morpheme.posTag()));
          }
        }
        tokens.add(object("term", term.toString(), "start_utf16", offsets.startOffset(),
            "end_utf16", offsets.endOffset(), "position", position,
            "position_increment", increment.getPositionIncrement(),
            "position_length", length.getPositionLength(), "pos_type", pos.getPOSType(),
            "left_pos", pos.getLeftPOS(), "right_pos", pos.getRightPOS(),
            "reading", reading.getReading(), "morphemes", morphemes));
      }
      stream.end();
      result.put("tokens", tokens);
      result.put("final_offset_utf16", offsets.endOffset());
      result.put("final_position_increment", increment.getPositionIncrement());
    } catch (IllegalArgumentException error) {
      result.put("error", object("class", error.getClass().getName(), "message", error.getMessage()));
    }
    System.out.println(json(result));
  }

  public static void main(String[] args) throws Exception {
    System.out.println(json(object("runtime", object(
        "java_version", System.getProperty("java.version"),
        "java_runtime_version", System.getProperty("java.runtime.version"),
        "java_vendor", System.getProperty("java.vendor")))));
    for (String mode : List.of("NONE", "DISCARD", "MIXED")) {
      emit(new Case("compound_" + mode, "가락지나물은 한국, 중국, 일본", "tokenizer", mode, false, true, null));
      emit(new Case("inflection_" + mode, "감싸여", "tokenizer", mode, false, true, null));
    }
    emit(new Case("default_analyzer", "가락지나물은 한국, 중국, 일본", "analyzer", "DISCARD", false, true, null));
    emit(new Case("mixed_analyzer", "가락지나물은 한국, 중국, 일본", "analyzer", "MIXED", false, true, null));
    emit(new Case("hanja_tokenizer", "喜悲哀歡", "tokenizer", "NONE", false, true, null));
    emit(new Case("hanja_analyzer", "喜悲哀歡", "analyzer", "DISCARD", false, true, null));
    for (boolean unigrams : List.of(false, true)) {
      emit(new Case("unknown_" + unigrams, "2018 평창 동계올림픽대회", "analyzer", "DISCARD", unigrams, true, null));
    }
    for (boolean discard : List.of(false, true)) {
      emit(new Case("punctuation_" + discard, "화학 이외의 것!", "tokenizer", "DISCARD", false, discard, null));
    }
    emit(new Case("user_compound", "세종시", "tokenizer", "MIXED", false, true, "세종시 세종 시\n"));
    emit(new Case("user_duplicate", "세종시", "tokenizer", "DISCARD", false, true, "세종시 세종 시\n세종시 세 종시\n"));
    emit(new Case("user_lengths", "세종시", "tokenizer", "DISCARD", false, true, "세종시 가나 다\n"));
    emit(new Case("user_short_segments", "세종시", "tokenizer", "DISCARD", false, true, "세종시 세종\n"));
    emit(new Case("user_long_segments", "세종시", "tokenizer", "DISCARD", false, true, "세종시 세종시 시\n"));
    emit(new Case("user_longest", "골드브라운", "tokenizer", "NONE", false, true, "골드\n브라운\n골드브라운\n"));
    emit(new Case("simple_lowercase", "İ ΟΣ UQA", "lowercase", "NONE", false, true, null));
    emit(new Case("supplementary", "𠀀🙂서울", "tokenizer", "DISCARD", true, true, null));
    emit(new Case("decomposed_hangul", "한글 한글", "analyzer", "DISCARD", false, true, null));
    emit(new Case("space_penalty", "먹 었다 먹  었다", "tokenizer", "DISCARD", false, true, null));
    emit(new Case("numbers", "３．２천", "number", "NONE", false, false, null));
    emit(new Case("numbers_without_punctuation", "３．２천", "number", "NONE", false, true, null));
    emit(new Case("numbers_mixed", "３．２천", "number", "MIXED", false, false, null));
    emit(new Case("number_comma", "15,7", "number", "NONE", false, false, null));
    emit(new Case("empty", "", "analyzer", "DISCARD", false, true, null));
    emit(new Case("trailing_stop", "나물은", "analyzer", "DISCARD", false, true, null));
    try (KoreanAnalyzer analyzer = new KoreanAnalyzer()) {
      System.out.println(json(object("id", "normalize", "input", "喜悲哀歡 İ UQA",
          "normalized", analyzer.normalize("body", "喜悲哀歡 İ UQA").utf8ToString())));
    }
  }
}
